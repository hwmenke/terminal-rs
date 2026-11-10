//! Relative rotation: archive observations, with an explicitly dated replay cursor.
use super::{PaneKind, TerminalApp};
use crate::theme::{self, amber, mono, muted, text};
use crate::{
    anim,
    congress_view::Action,
    dock::PaneId,
    worker::{Request, Worker},
};
use egui::{Align2, Color32, Key, Mesh, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2, pos2, vec2};
use tr_data::rotation::{self, Point, Quadrant, RotationReport, TAILS, Track};

#[derive(Default)]
pub struct LabelsCache {
    key: Option<(usize, Rect, Rect, [bool; 4])>,
    offsets: Vec<Option<Vec2>>,
}
pub struct RotationPane {
    pub spec: String,
    pub tail: usize,
    pub show_table: bool,
    pub generation: u64,
    pub report: Option<RotationReport>,
    pub loading: bool,
    pub error: Option<String>,
    pub cursor: f32,
    pub playing: bool,
    pub selected: Option<usize>,
    pub quad_mask: [bool; 4],
    /// Manual bounds in ratio/momentum coordinates (Y grows upwards).
    pub view: Option<Rect>,
    pub labels_cache: LabelsCache,
    sort: usize,
    descending: bool,
    play_clock: f32,
    loading_names: usize,
    tail_palette: Option<([Color32; 4], egui::TextureHandle)>,
}
impl RotationPane {
    pub fn new(spec: impl Into<String>) -> Self {
        let spec = spec.into();
        let tail = rotation::parse_query(&spec, &["AAPL".into()]).map(|q| q.tail).unwrap_or(10);
        Self {
            spec,
            tail,
            show_table: true,
            generation: 0,
            report: None,
            loading: false,
            error: None,
            cursor: 0.0,
            playing: false,
            selected: None,
            quad_mask: [true; 4],
            view: None,
            labels_cache: LabelsCache::default(),
            sort: 0,
            descending: false,
            play_clock: 0.0,
            loading_names: 12,
            tail_palette: None,
        }
    }
    pub fn request(&mut self, pane: PaneId, worker: &Worker, watchlist: &[String]) {
        // Invalid requests also invalidate older in-flight results.
        self.generation += 1;
        self.report = None;
        self.error = None;
        self.loading = false;
        self.labels_cache = LabelsCache::default();
        match rotation::parse_query(&self.spec, watchlist) {
            Ok(mut query) => {
                self.loading_names = query.symbols.len() + 1;
                query.tail = self.tail;
                self.loading = true;
                worker.send(Request::Rotation { pane, generation: self.generation, query });
            }
            Err(e) => self.error = Some(e),
        }
    }
    pub fn accept(&mut self, generation: u64, result: Result<RotationReport, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        self.labels_cache = LabelsCache::default();
        self.selected = None;
        match result {
            Ok(report) => {
                if let Err(e) = report.provenance.require_verified("Rotation") {
                    self.error = Some(e);
                    self.report = None;
                    self.playing = false;
                    return;
                }
                let last = report.dates.len().saturating_sub(1) as f32;
                self.cursor = if self.playing { self.cursor.clamp(0.0, last) } else { last };
                self.error = if report.tracks.len() < 2 {
                    Some(format!(
                        "Rotation refused: needs at least 2 plottable names, has {}. See skipped names below.",
                        report.tracks.len()
                    ))
                } else {
                    None
                };
                if self.error.is_some() {
                    self.playing = false;
                }
                self.report = Some(report);
            }
            Err(e) => {
                self.error = Some(e);
                self.report = None;
                self.playing = false;
            }
        }
    }
    fn last(&self) -> f32 {
        self.report.as_ref().map(|r| r.dates.len().saturating_sub(1) as f32).unwrap_or(0.0)
    }
    fn step(&mut self, by: f32) {
        self.cursor = (self.cursor + by).clamp(0.0, self.last());
        self.playing = false;
    }
    fn key(&mut self, key: Key, shift: bool) -> bool {
        match key {
            Key::Space => {
                if !self.playing && self.cursor >= self.last() {
                    self.cursor = 0.0;
                }
                self.playing = !self.playing;
                self.play_clock = 0.0;
            }
            Key::ArrowLeft => self.step(if shift { -5.0 } else { -1.0 }),
            Key::ArrowRight => self.step(if shift { 5.0 } else { 1.0 }),
            Key::Home => {
                self.cursor = 0.0;
                self.playing = false;
            }
            Key::End => {
                self.cursor = self.last();
                self.playing = false;
            }
            Key::T => {
                let i = TAILS.iter().position(|t| *t == self.tail).unwrap_or(1);
                self.tail = TAILS[(i + if shift { 4 } else { 1 }) % 5];
            }
            Key::L => self.show_table = !self.show_table,
            Key::Num0 => self.quad_mask = [true; 4],
            Key::Num1 | Key::Num2 | Key::Num3 | Key::Num4 => {
                let i = match key {
                    Key::Num1 => 0,
                    Key::Num2 => 1,
                    Key::Num3 => 2,
                    _ => 3,
                };
                self.quad_mask[i] = !self.quad_mask[i];
            }
            Key::Escape => self.selected = None,
            Key::J | Key::ArrowDown | Key::K | Key::ArrowUp => {
                if let Some(r) = &self.report {
                    let mut names = visible(r, self.cursor, self.quad_mask);
                    sort_rows(r, &mut names, self.cursor, self.sort, self.descending);
                    if !names.is_empty() {
                        let next = matches!(key, Key::J | Key::ArrowDown);
                        let i = self.selected.and_then(|s| names.iter().position(|n| *n == s));
                        let i = match i {
                            Some(i) => (i + if next { 1 } else { names.len() - 1 }) % names.len(),
                            None => {
                                if next {
                                    0
                                } else {
                                    names.len() - 1
                                }
                            }
                        };
                        self.selected = Some(names[i]);
                    }
                }
            }
            _ => return false,
        }
        true
    }
    fn advance(&mut self, dt: f32) {
        if self.playing && self.report.is_some() && self.error.is_none() {
            if anim::reduced() {
                self.play_clock += dt * 6.0;
                let n = self.play_clock.floor();
                self.play_clock -= n;
                self.cursor = self.cursor.floor() + n;
            } else {
                self.cursor += dt * 6.0;
            }
            if self.cursor >= self.last() {
                self.cursor = self.last();
                self.playing = false;
            }
        }
    }
}
pub(super) fn prepend_symbol(symbol: Option<&str>, spec: &str) -> String {
    match symbol {
        Some(s) => {
            let s = tr_data::security(s).ticker;
            format!("{s} {spec}").trim().to_string()
        }
        None => spec.to_string(),
    }
}
impl TerminalApp {
    pub(super) fn rotation_watchlist(&self, spec: &str) -> Vec<String> {
        let group = rotation::watchlist_group(spec)
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| self.watch_ui.selected_group.clone());
        self.watchlists.group(&group).map(|g| g.members.iter().map(|m| m.symbol.clone()).collect()).unwrap_or_default()
    }
    pub(super) fn reload_rotation_panes(&mut self) {
        let panes: Vec<_> = self
            .panes
            .iter()
            .filter_map(|p| {
                if let PaneKind::Rotation(r) = &p.kind { Some((p.id, self.rotation_watchlist(&r.spec))) } else { None }
            })
            .collect();
        for (id, symbols) in panes {
            if let Some(i) = self.idx(id)
                && let PaneKind::Rotation(p) = &mut self.panes[i].kind
            {
                p.request(id, &self.worker, &symbols);
            }
        }
    }
    pub(super) fn open_rotation(&mut self, symbol: Option<&str>, spec: &str) {
        self.status = "Rotation · verified daily research archive".into();
        self.open_panel(PaneKind::Rotation(Box::new(RotationPane::new(prepend_symbol(symbol, spec)))));
    }
    pub(super) fn rotation_keys(&mut self, ctx: &egui::Context) -> bool {
        let Some(i) = self.idx(self.focus) else { return false };
        if !matches!(self.panes[i].kind, PaneKind::Rotation(_)) {
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
        let mut consumed = false;
        let mut reload = false;
        let mut add = None;
        let mut open = None;
        for (key, m) in keys {
            if m.command || m.ctrl || m.alt {
                continue;
            }
            let PaneKind::Rotation(p) = &mut self.panes[i].kind else { break };
            let handled = match key {
                Key::R => {
                    reload = true;
                    true
                }
                Key::A => {
                    add = archive_add(p);
                    true
                }
                Key::Slash => {
                    self.focus_cmd = true;
                    true
                }
                Key::Enter => {
                    open = selected_symbol(p);
                    true
                }
                _ => p.key(key, m.shift),
            };
            if handled {
                consumed = true;
                ctx.input_mut(|i| {
                    i.consume_key(m, key);
                    i.events.retain(
                        |e| !matches!(e,egui::Event::Text(t) if t.chars().all(|c|"tTlLrRaAjJkK01234/ ".contains(c))),
                    );
                });
            }
        }
        if reload {
            let spec = if let PaneKind::Rotation(p) = &self.panes[i].kind { p.spec.clone() } else { String::new() };
            let symbols = self.rotation_watchlist(&spec);
            let id = self.panes[i].id;
            if let PaneKind::Rotation(p) = &mut self.panes[i].kind {
                p.request(id, &self.worker, &symbols);
            }
        }
        if let Some(cmd) = add {
            self.execute(&cmd);
        }
        if let Some(symbol) = open {
            self.open_list_symbol(&symbol, true, None);
        }
        consumed
    }
    pub(super) fn rotation_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("rotation pane");
        let mut reload = false;
        let mut add = None;
        let mut open = None;
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        if let PaneKind::Rotation(p) = &mut self.panes[i].kind {
            p.advance(ui.input(|i| i.stable_dt).min(0.1));
            if p.playing && p.report.is_some() && p.error.is_none() {
                ui.ctx().request_repaint();
            }
            ui.horizontal_wrapped(|ui| {
                let label = p
                    .report
                    .as_ref()
                    .map(|r| {
                        format!(
                            "Rotation {}{} ({}) vs {} · {} {}/{}/{} · tail {}",
                            r.query.symbols.iter().take(3).cloned().collect::<Vec<_>>().join(" "),
                            if r.query.symbols.len() > 3 { " …" } else { "" },
                            r.query.symbols.len(),
                            r.query.benchmark,
                            if r.query.cadence == rotation::Cadence::Daily { "DAILY" } else { "WEEKLY" },
                            r.params.smooth,
                            r.params.lookback,
                            r.params.momentum,
                            p.tail
                        )
                    })
                    .unwrap_or_else(|| "Rotation · research archive".into());
                ui.label(RichText::new(label).font(mono(11.0)).color(text()))
                    .on_hover_text("Open formula, not JdK RS-Ratio®");
                if let Some(r) = &p.report {
                    let stale = r.stale_sessions > 0;
                    ui.label(
                        RichText::new(if stale {
                            format!("STALE {} sessions", r.stale_sessions)
                        } else {
                            "VERIFIED".into()
                        })
                        .font(mono(11.0))
                        .color(if stale { amber() } else { theme::up() }),
                    );
                    ui.label(
                        RichText::new(format!("as of {} · {}", r.as_of, r.source)).font(mono(11.0)).color(muted()),
                    );
                    if r.week_in_progress {
                        ui.label(RichText::new("WTD").font(mono(11.0)).color(amber()));
                    }
                }
                if ui.small_button("Reload [R]").clicked() {
                    reload = true;
                }
            });
            if sample_offline {
                ui.label(RichText::new("Sample / offline display mode does not feed this pane. Rotation never falls back to sample bars.").font(mono(12.0)).color(theme::down()));
            }
            let height = (ui.available_height() - 88.0).max(100.0);
            ui.horizontal_top(|ui| {
                let width = if p.show_table { ui.available_width() * 0.70 } else { ui.available_width() };
                open = paint_stage(ui, p, id, vec2(width, height));
                if p.show_table
                    && let Some(symbol) = paint_table(ui, p, height)
                {
                    open = Some(symbol);
                }
            });
            ui.horizontal(|ui| {
                if ui.small_button("◀").clicked() {
                    p.step(-1.0);
                }
                if ui.small_button(if p.playing { "Ⅱ" } else { "▷" }).clicked() {
                    p.key(Key::Space, false);
                }
                if ui.small_button("▶").clicked() {
                    p.step(1.0);
                }
                let last = p.last();
                let replay = p.cursor < last;
                if let Some(r) = &p.report {
                    let at = (p.cursor.floor() as usize).min(r.dates.len().saturating_sub(1));
                    if let Some(d) = r.dates.get(at) {
                        ui.label(
                            RichText::new(format!("{d} (frame {}/{})", at + 1, r.dates.len()))
                                .font(mono(11.0))
                                .color(if replay { amber() } else { muted() }),
                        );
                    }
                    if replay {
                        ui.label(RichText::new("REPLAY").font(mono(11.0)).color(amber()));
                    }
                }
                let slider = egui::Slider::new(&mut p.cursor, 0.0..=last).show_value(false);
                if ui.add_sized([ui.available_width().max(40.0), 20.0], slider).changed() {
                    p.playing = false;
                }
            });
            if let Some(r) = &p.report {
                if r.stale_sessions > 0 {
                    ui.label(
                        RichText::new("HIST REFRESH · stale archive; hollow heads are dated observations")
                            .font(mono(11.0))
                            .color(amber()),
                    );
                }
                if !r.skipped.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "skipped: {}",
                                r.skipped
                                    .iter()
                                    .map(|s| format!("{} {}", s.symbol, s.reason))
                                    .collect::<Vec<_>>()
                                    .join(" · ")
                            ))
                            .font(mono(11.0))
                            .color(muted()),
                        );
                        if archive_add(p).is_some() && ui.small_button("[A] add to HIST").clicked() {
                            add = archive_add(p);
                        }
                    });
                }
            }
        }
        if reload {
            let spec = if let PaneKind::Rotation(p) = &self.panes[i].kind { p.spec.clone() } else { String::new() };
            let symbols = self.rotation_watchlist(&spec);
            if let PaneKind::Rotation(p) = &mut self.panes[i].kind {
                p.request(id, &self.worker, &symbols);
            }
        }
        if let Some(symbol) = open {
            self.open_list_symbol(&symbol, true, None);
        }
        add.map(|s| vec![Action::Command(s)]).unwrap_or_default()
    }
}
fn archive_add(p: &RotationPane) -> Option<String> {
    let names = p
        .report
        .as_ref()?
        .skipped
        .iter()
        .filter(|s| s.reason == "not in archive")
        .map(|s| s.symbol.clone())
        .collect::<Vec<_>>();
    if names.is_empty() { None } else { Some(format!("HIST ADD {}", names.join(" "))) }
}
fn selected_symbol(p: &RotationPane) -> Option<String> {
    let r = p.report.as_ref()?;
    let t = r.tracks.get(p.selected?)?;
    let head = current(t, p.cursor)?;
    p.quad_mask[rotation::quadrant(head.point).index()].then(|| t.symbol.clone())
}
fn color(q: Quadrant) -> Color32 {
    match q {
        Quadrant::Leading => theme::up(),
        Quadrant::Weakening => amber(),
        Quadrant::Lagging => theme::down(),
        Quadrant::Improving => theme::cyan(),
    }
}
#[derive(Clone, Copy)]
struct Head {
    point: Point,
    at: usize,
    lagged: bool,
}
fn current(t: &Track, cursor: f32) -> Option<Head> {
    let at = (cursor.floor() as usize).min(t.points.len().checked_sub(1)?);
    let i = (0..=at).rev().find(|i| t.points[*i].is_some())?;
    let mut point = t.points[i]?;
    if i == at
        && cursor.fract() > 0.0
        && let Some(Some(next)) = t.points.get(i + 1)
    {
        let f = cursor.fract();
        point = Point {
            ratio: point.ratio + (next.ratio - point.ratio) * f,
            momentum: point.momentum + (next.momentum - point.momentum) * f,
        };
    }
    Some(Head { point, at: i, lagged: i != at })
}
fn visible(r: &RotationReport, cursor: f32, mask: [bool; 4]) -> Vec<usize> {
    r.tracks
        .iter()
        .enumerate()
        .filter_map(|(i, t)| current(t, cursor).filter(|h| mask[rotation::quadrant(h.point).index()]).map(|_| i))
        .collect()
}
fn transform(p: Point, plot: Rect, view: Rect) -> Pos2 {
    pos2(
        plot.left() + (p.ratio - view.left()) / view.width() * plot.width(),
        plot.bottom() - (p.momentum - view.top()) / view.height() * plot.height(),
    )
}
fn untransform(p: Pos2, plot: Rect, view: Rect) -> Pos2 {
    pos2(
        view.left() + (p.x - plot.left()) / plot.width() * view.width(),
        view.top() + (plot.bottom() - p.y) / plot.height() * view.height(),
    )
}
fn tail_start(at: usize, tail: usize) -> usize {
    (at + 1).saturating_sub(tail)
}
fn paint_stage(ui: &mut Ui, p: &mut RotationPane, id: PaneId, size: Vec2) -> Option<String> {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let plot = Rect::from_min_max(rect.min + vec2(34.0, 12.0), rect.max - vec2(12.0, 24.0));
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 4.0, theme::card());
    let can_plot = p.report.is_some() && p.error.is_none() && !p.loading;
    let names = if can_plot { visible(p.report.as_ref().unwrap(), p.cursor, p.quad_mask) } else { vec![] };
    let mut span = 2.0f32;
    if let Some(r) = &p.report
        && can_plot
    {
        for &i in &names {
            let t = &r.tracks[i];
            if let Some(h) = current(t, p.cursor) {
                for point in t.points[tail_start(h.at, p.tail)..=h.at].iter().flatten().chain(std::iter::once(&h.point))
                {
                    span = span.max((point.ratio - 100.0).abs() * 1.08).max((point.momentum - 100.0).abs() * 1.08);
                }
            }
        }
    }
    if p.view.is_none() {
        span = anim::spring(ui.ctx(), response.id.with("span"), span).max(2.0);
    }
    let mut view = p
        .view
        .unwrap_or_else(|| Rect::from_min_max(pos2(100.0 - span, 100.0 - span), pos2(100.0 + span, 100.0 + span)));
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0
            && let Some(mouse) = response.hover_pos()
        {
            let anchor = untransform(mouse, plot, view);
            let factor = (-scroll * 0.002).exp().clamp(0.5, 2.0);
            let min = anchor + (view.min - anchor) * factor;
            let max = anchor + (view.max - anchor) * factor;
            if max.x - min.x > 0.05 && max.x - min.x < 10000.0 {
                view = Rect::from_min_max(min, max);
                p.view = Some(view);
            }
        }
    }
    if response.dragged() {
        let delta = ui.input(|i| i.pointer.delta());
        let offset = vec2(-delta.x / plot.width() * view.width(), delta.y / plot.height() * view.height());
        view = view.translate(offset);
        p.view = Some(view);
    }
    let center = transform(Point { ratio: 100.0, momentum: 100.0 }, plot, view);
    let cx = center.x.clamp(plot.left(), plot.right());
    let cy = center.y.clamp(plot.top(), plot.bottom());
    let corners = [
        (Quadrant::Improving, Rect::from_min_max(plot.min, pos2(cx, cy)), Align2::LEFT_TOP),
        (Quadrant::Leading, Rect::from_min_max(pos2(cx, plot.top()), pos2(plot.right(), cy)), Align2::RIGHT_TOP),
        (Quadrant::Lagging, Rect::from_min_max(pos2(plot.left(), cy), pos2(cx, plot.bottom())), Align2::LEFT_BOTTOM),
        (Quadrant::Weakening, Rect::from_min_max(pos2(cx, cy), plot.max), Align2::RIGHT_BOTTOM),
    ];
    let mut wash = Mesh::default();
    for (q, r, _) in &corners {
        wash.add_colored_rect(*r, color(*q).gamma_multiply(0.06));
    }
    painter.add(egui::Shape::mesh(wash));
    for (q, r, align) in corners {
        if r.width() > 60.0 && r.height() > 24.0 {
            let x = if align == Align2::RIGHT_TOP || align == Align2::RIGHT_BOTTOM {
                r.right() - 8.0
            } else {
                r.left() + 8.0
            };
            let y = if align == Align2::LEFT_BOTTOM || align == Align2::RIGHT_BOTTOM {
                r.bottom() - 8.0
            } else {
                r.top() + 8.0
            };
            painter.text(pos2(x, y), align, q.label().to_ascii_uppercase(), mono(11.0), color(q).gamma_multiply(0.55));
        }
    }
    for k in 0..=8 {
        let value = view.left() + view.width() * k as f32 / 8.0;
        let x = plot.left() + plot.width() * k as f32 / 8.0;
        painter.vline(x, plot.y_range(), Stroke::new(0.5, theme::grid()));
        painter.text(pos2(x, plot.bottom() + 4.0), Align2::CENTER_TOP, format!("{value:.1}"), mono(10.0), muted());
        let value = view.top() + view.height() * k as f32 / 8.0;
        let y = plot.bottom() - plot.height() * k as f32 / 8.0;
        painter.hline(plot.x_range(), y, Stroke::new(0.5, theme::grid()));
        painter.text(pos2(plot.left() - 4.0, y), Align2::RIGHT_CENTER, format!("{value:.1}"), mono(10.0), muted());
    }
    if center.x >= plot.left() && center.x <= plot.right() {
        painter.vline(center.x, plot.y_range(), Stroke::new(1.5, theme::border()));
    }
    if center.y >= plot.top() && center.y <= plot.bottom() {
        painter.hline(plot.x_range(), center.y, Stroke::new(1.5, theme::border()));
    }
    if !can_plot {
        let message = p.error.clone().unwrap_or_else(|| {
            if p.loading {
                let count = p.loading_names;
                format!("Loading {count} names from the research archive…")
            } else {
                "Reading research archive…".into()
            }
        });
        let text_rect = Rect::from_center_size(plot.center(), vec2((plot.width() - 32.0).max(40.0), 100.0));
        ui.put(
            text_rect,
            egui::Label::new(RichText::new(message).font(mono(12.0)).color(if p.error.is_some() {
                theme::down()
            } else {
                muted()
            }))
            .wrap(),
        );
        return None;
    }
    let r = p.report.as_ref().unwrap();
    let draw = anim::ease_out(anim::appear(ui.ctx(), response.id.with(("draw", p.generation)), 0.350));
    let mut hover: Option<(usize, usize, Point)> = None;
    let mut nearest = 8.0f32;
    if let Some(mouse) = response.hover_pos().filter(|m| plot.contains(*m)) {
        for &i in &names {
            let t = &r.tracks[i];
            let Some(h) = current(t, p.cursor) else { continue };
            let start = tail_start(h.at, p.tail);
            let mut prev: Option<(Pos2, usize, Point)> = None;
            for at in start..=h.at {
                if let Some(point) = t.points[at] {
                    let pos = transform(point, plot, view);
                    let distance = pos.distance(mouse);
                    if distance < nearest {
                        nearest = distance;
                        hover = Some((i, at, point));
                    }
                    if let Some((old, old_at, old_point)) = prev {
                        let delta: Vec2 = pos - old;
                        let f = if delta.length_sq() > 0.0 {
                            ((mouse - old).dot(delta) / delta.length_sq()).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let distance = (old + delta * f).distance(mouse);
                        if distance < nearest {
                            nearest = distance;
                            hover = Some(if f < 0.5 { (i, old_at, old_point) } else { (i, at, point) });
                        }
                    }
                    prev = Some((pos, at, point));
                } else {
                    prev = None;
                }
            }
            let distance = transform(h.point, plot, view).distance(mouse);
            if distance < nearest {
                nearest = distance;
                hover = Some((i, h.at, h.point));
            }
        }
    }
    if response.clicked() {
        p.selected = hover.map(|h| h.0);
    }
    let emphasis = hover.map(|h| h.0).or(p.selected.filter(|s| names.contains(s)));
    let painter = painter.with_clip_rect(plot);
    // A nearest-filtered 2×2 palette gives exact quadrant boundaries without
    // splitting geometry at every crossing (at most 4,800 vertices, even at 40×30).
    let colors = [theme::down(), amber(), theme::cyan(), theme::up()];
    if p.tail_palette.as_ref().is_none_or(|(old, _)| *old != colors) {
        p.tail_palette = Some((
            colors,
            ui.ctx().load_texture(
                format!("rotation-tail-{id}"),
                egui::ColorImage::new([2, 2], colors.to_vec()),
                egui::TextureOptions::NEAREST,
            ),
        ));
    }
    let mut tails = Mesh::with_texture(p.tail_palette.as_ref().unwrap().1.id());
    for &i in &names {
        let t = &r.tracks[i];
        let Some(h) = current(t, p.cursor) else { continue };
        let dim = if emphasis.is_some_and(|e| e != i) { 0.25 } else { 1.0 };
        let start = tail_start(h.at, p.tail);
        let end = start + ((h.at - start) as f32 * draw).ceil() as usize;
        let mut prev: Option<(Point, f32)> = None;
        for at in start..=end.min(h.at) {
            let f = (at - start) as f32 / (h.at - start).max(1) as f32;
            if let Some(point) = t.points[at] {
                if let Some((old, old_f)) = prev {
                    tail_segment(&mut tails, old, point, old_f, f, dim, plot, view);
                }
                prev = Some((point, f));
            } else {
                prev = None;
            }
        }
        if draw >= 1.0
            && p.cursor.fract() > 0.0
            && !h.lagged
            && let Some((old, f)) = prev
        {
            tail_segment(&mut tails, old, h.point, f, 1.0, dim, plot, view);
        }
    }
    painter.add(egui::Shape::mesh(tails));
    let cache_key = (p.cursor.floor() as usize, plot, view, p.quad_mask);
    if p.labels_cache.key != Some(cache_key) {
        p.labels_cache.key = Some(cache_key);
        p.labels_cache.offsets = vec![None; r.tracks.len()];
        let mut used: Vec<Rect> = names
            .iter()
            .filter_map(|&i| {
                current(&r.tracks[i], p.cursor)
                    .map(|h| Rect::from_center_size(transform(h.point, plot, view), vec2(20.0, 20.0)))
            })
            .collect();
        for &i in &names {
            let t = &r.tracks[i];
            let h = current(t, p.cursor).unwrap();
            let pos = transform(h.point, plot, view);
            let galley = painter.layout_no_wrap(t.symbol.clone(), mono(11.0), text());
            let size = galley.size();
            for offset in [
                vec2(12.0, -size.y / 2.0),
                vec2(-12.0 - size.x, -size.y / 2.0),
                vec2(-size.x / 2.0, -12.0 - size.y),
                vec2(-size.x / 2.0, 12.0),
            ] {
                let candidate = Rect::from_min_size(pos + offset, size).expand(2.0);
                if plot.contains_rect(candidate) && !used.iter().any(|r| r.intersects(candidate)) {
                    p.labels_cache.offsets[i] = Some(offset);
                    used.push(candidate);
                    break;
                }
            }
        }
    }
    for &i in &names {
        let t = &r.tracks[i];
        let Some(h) = current(t, p.cursor) else { continue };
        let pos = transform(h.point, plot, view);
        let dim = if emphasis.is_some_and(|e| e != i) { 0.25 } else { 1.0 };
        let c = color(rotation::quadrant(h.point)).gamma_multiply(dim);
        painter.circle_stroke(pos, 9.0, Stroke::new(2.0, c.gamma_multiply(0.25)));
        if r.stale_sessions > 0 || t.stale_sessions > 0 || h.lagged || t.gaps > 0 {
            painter.circle_stroke(pos, 4.5, Stroke::new(1.5, c));
        } else {
            painter.circle_filled(pos, 4.5, c);
        }
        if emphasis == Some(i) {
            for at in tail_start(h.at, p.tail)..=h.at {
                if let Some(point) = t.points[at] {
                    painter.circle_filled(transform(point, plot, view), 2.0, color(rotation::quadrant(point)));
                }
            }
        }
        if let Some(offset) =
            p.labels_cache.offsets[i].or_else(|| (hover.is_some_and(|h| h.0 == i)).then_some(vec2(12.0, -7.0)))
        {
            painter.text(pos + offset, Align2::LEFT_TOP, &t.symbol, mono(11.0), text().gamma_multiply(dim));
        }
    }
    if let Some((i, at, point)) = hover {
        ui.ctx().request_repaint();
        response.clone().on_hover_text(hover_text(r, i, at, point));
    }
    if response.double_clicked() {
        if let Some((i, _, _)) = hover {
            return Some(r.tracks[i].symbol.clone());
        }
        p.view = None;
    }
    None
}
/// UVs interpolate in ratio/momentum space; nearest sampling switches palette
/// texels exactly at 100/100, including when both axes cross in a single step.
fn tail_segment(mesh: &mut Mesh, a: Point, b: Point, fa: f32, fb: f32, dim: f32, plot: Rect, view: Rect) {
    let pa = transform(a, plot, view);
    let pb = transform(b, plot, view);
    let delta = pb - pa;
    if delta.length_sq() < 1e-8 {
        return;
    }
    let normal = vec2(-delta.y, delta.x).normalized() * 0.5;
    let base = mesh.vertices.len() as u32;
    for (p, pos, f) in [(a, pa, fa), (b, pb, fb)] {
        let half = normal * (1.0 + 2.0 * f);
        let uv = pos2(0.5 + (p.ratio - 100.0) / view.width(), 0.5 + (p.momentum - 100.0) / view.height());
        let color = Color32::WHITE.gamma_multiply((0.1 + 0.9 * f) * dim);
        mesh.vertices.push(egui::epaint::Vertex { pos: pos - half, uv, color });
        mesh.vertices.push(egui::epaint::Vertex { pos: pos + half, uv, color });
    }
    mesh.add_triangle(base, base + 1, base + 2);
    mesh.add_triangle(base + 1, base + 3, base + 2);
}
fn hover_text(r: &RotationReport, i: usize, at: usize, point: Point) -> String {
    let t = &r.tracks[i];
    let q = rotation::quadrant(point);
    let n = quadrant_age(t, Head { point, at, lagged: false });
    let entered = at + 1 - n.max(1);
    format!(
        "{} · {} · ratio {:.2} · mom {:.2} · {} · entered {} {}",
        t.symbol,
        q.label(),
        point.ratio,
        point.momentum,
        r.dates[at],
        q.label(),
        r.dates[entered]
    )
}
fn quadrant_age(t: &Track, h: Head) -> usize {
    if t.points[h.at].is_some_and(|p| rotation::quadrant(p) != rotation::quadrant(h.point)) {
        1
    } else {
        rotation::frames_in_quadrant(t, h.at)
    }
}
fn speed(t: &Track, h: Head) -> Option<f32> {
    h.at.checked_sub(1)
        .and_then(|i| t.points[i])
        .map(|p| (h.point.ratio - p.ratio).hypot(h.point.momentum - p.momentum))
}
fn sort_rows(r: &RotationReport, names: &mut [usize], cursor: f32, sort: usize, descending: bool) {
    names.sort_by(|a, b| {
        let ta = &r.tracks[*a];
        let tb = &r.tracks[*b];
        let ha = current(ta, cursor).unwrap();
        let hb = current(tb, cursor).unwrap();
        let order = match sort {
            1 => rotation::quadrant(ha.point).index().cmp(&rotation::quadrant(hb.point).index()),
            2 => ha.point.ratio.total_cmp(&hb.point.ratio),
            3 => ha.point.momentum.total_cmp(&hb.point.momentum),
            4 => heading_at(ta, ha).cmp(&heading_at(tb, hb)),
            5 => speed(ta, ha).partial_cmp(&speed(tb, hb)).unwrap_or(std::cmp::Ordering::Equal),
            6 => quadrant_age(ta, ha).cmp(&quadrant_age(tb, hb)),
            _ => ta.symbol.cmp(&tb.symbol),
        };
        if descending { order.reverse() } else { order }
    });
}
fn heading_at(t: &Track, h: Head) -> Option<u8> {
    h.at.checked_sub(1).and_then(|i| t.points[i]).map(|p| rotation::heading(p, h.point))
}
fn paint_table(ui: &mut Ui, p: &mut RotationPane, height: f32) -> Option<String> {
    let Some(r) = &p.report else { return None };
    if p.error.is_some() {
        return None;
    }
    let mut names = visible(r, p.cursor, p.quad_mask);
    sort_rows(r, &mut names, p.cursor, p.sort, p.descending);
    let mut open = None;
    egui::ScrollArea::both().id_salt("rotation-table").max_height(height).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (i, label) in ["SYM", "Q", "RATIO", "MOM", "HDG", "SPD", "IN-Q"].iter().enumerate() {
                if ui.small_button(RichText::new(*label).font(mono(10.0))).clicked() {
                    if p.sort == i {
                        p.descending = !p.descending;
                    } else {
                        p.sort = i;
                        p.descending = false;
                    }
                }
            }
        });
        for i in names {
            let t = &r.tracks[i];
            let h = current(t, p.cursor).unwrap();
            let q = rotation::quadrant(h.point);
            let hdg = heading_at(t, h).map(|d| ["↑", "↗", "→", "↘", "↓", "↙", "←", "↖"][d as usize]).unwrap_or("·");
            let in_q = quadrant_age(t, h);
            let format = egui::TextFormat { font_id: mono(11.0), color: text(), ..Default::default() };
            let mut line = egui::text::LayoutJob::default();
            line.append(&format!("{:<5} ", t.symbol), 0.0, format.clone());
            line.append(
                &format!("{} ", q.label().chars().next().unwrap()),
                0.0,
                egui::TextFormat { color: color(q), ..format.clone() },
            );
            line.append(
                &format!(
                    "{:>6.2} {:>6.2}  {} {:>5} {:>3}{}",
                    h.point.ratio,
                    h.point.momentum,
                    hdg,
                    speed(t, h).map(|s| format!("{s:.2}")).unwrap_or_else(|| "—".into()),
                    in_q,
                    if r.query.cadence == rotation::Cadence::Daily { "d" } else { "w" }
                ),
                0.0,
                format,
            );
            let selected = p.selected == Some(i);
            let response = egui::Frame::new()
                .fill(if selected { theme::accent_wash() } else { Color32::TRANSPARENT })
                .show(ui, |ui| {
                    let response = ui.add(egui::Label::new(line).sense(Sense::click()));
                    if h.lagged || t.stale_sessions > 0 || t.gaps > 0 {
                        ui.label(
                            RichText::new(format!("{} · dated observation", r.dates[h.at]))
                                .font(mono(10.0))
                                .color(amber()),
                        );
                    }
                    response
                })
                .inner;
            if response.clicked() {
                p.selected = Some(i);
            }
            if response.double_clicked() {
                p.selected = Some(i);
                open = Some(t.symbol.clone());
            }
            response.on_hover_text(hover_text(r, i, h.at, h.point));
        }
    });
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use tr_core::{Bar, Provenance};
    fn report() -> RotationReport {
        let d = chrono::NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
        let bars: Vec<_> = (0..400)
            .map(|i| {
                Bar::new(
                    (d + chrono::Duration::days(i)).and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(),
                    100.0,
                    101.0,
                    99.0,
                    100.0,
                    100.0,
                )
                .with_provenance(Provenance::Real)
            })
            .collect();
        rotation::build_report(
            &bars,
            vec![("AAPL".into(), Ok(bars.clone())), ("MSFT".into(), Ok(bars.clone()))],
            &rotation::parse_query("AAPL MSFT", &[]).unwrap(),
            "Local Yahoo daily research archive",
            d + chrono::Duration::days(399),
        )
        .unwrap()
    }
    #[test]
    fn rrg_commands_open_the_pane_and_request_the_list() {
        let mut app = test_app();
        app.execute("AAPL RRG MSFT VS QQQ WEEKLY TAIL 20");
        let pane =
            app.panes.iter().find_map(|p| if let PaneKind::Rotation(r) = &p.kind { Some(r) } else { None }).unwrap();
        assert_eq!(pane.spec, "AAPL MSFT VS QQQ WEEKLY TAIL 20");
        assert!(pane.loading);
        assert_eq!(pane.tail, 20);
        let (worker, mut requests) = Worker::test_channel();
        let mut p = RotationPane::new("WL Growth VS QQQ WEEKLY");
        p.request(7, &worker, &["AAPL".into(), "MSFT".into()]);
        match requests.try_recv().unwrap() {
            Request::Rotation { pane, generation, query } => {
                assert_eq!(pane, 7);
                assert_eq!(generation, 1);
                assert_eq!(query.symbols, vec!["AAPL", "MSFT"]);
                assert_eq!(query.benchmark, "QQQ");
                assert_eq!(query.cadence, rotation::Cadence::Weekly);
            }
            _ => panic!("wrong request"),
        }
    }
    #[test]
    fn rotation_pane_rejects_unverified_reports() {
        let mut r = report();
        r.provenance = Provenance::Sample;
        let mut p = RotationPane::new("");
        p.accept(0, Ok(r));
        assert!(p.report.is_none());
        assert!(p.error.unwrap().contains("refused"));
    }
    #[test]
    fn stale_generation_is_ignored() {
        let mut p = RotationPane::new("");
        p.generation = 2;
        p.loading = true;
        p.accept(1, Ok(report()));
        assert!(p.report.is_none());
        assert!(p.loading);
    }
    #[test]
    fn cursor_clamps_and_space_toggles_play() {
        let mut p = RotationPane::new("");
        p.playing = true;
        p.cursor = 10000.0;
        p.accept(0, Ok(report()));
        assert_eq!(p.cursor, 259.0);
        p.playing = false;
        p.key(Key::Space, false);
        assert!(p.playing);
        assert_eq!(p.cursor, 0.0);
        p.advance(0.1);
        assert!((p.cursor - 0.6).abs() < 1e-4);
        p.key(Key::Space, false);
        assert!(!p.playing);
        p.key(Key::End, false);
        assert_eq!(p.cursor, 259.0);
        p.key(Key::ArrowRight, true);
        assert_eq!(p.cursor, 259.0);
        p.key(Key::Home, false);
        p.key(Key::ArrowLeft, false);
        assert_eq!(p.cursor, 0.0);
    }
    #[test]
    fn rotation_keys_keep_focus_and_chart_links() {
        let mut app = test_app();
        app.execute("NEW CHART AAPL");
        let chart = app.focus;
        app.pane_mut(chart).unwrap().link = Some(crate::layouts::Link::Blue);
        app.execute("SPLIT DOWN CHART NVDA");
        app.pane_mut(app.focus).unwrap().link = Some(crate::layouts::Link::Blue);
        app.watch_ui.active_chart = Some(chart);
        app.execute("NEW RRG");
        let id = app.focus;
        let idx = app.idx(id).unwrap();
        let PaneKind::Rotation(p) = &mut app.panes[idx].kind else { panic!() };
        p.accept(p.generation, Ok(report()));
        p.selected = Some(1);
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        ctx.run_ui(input, |ui| assert!(app.rotation_keys(ui.ctx()))).textures_delta.clear();
        assert_eq!(app.focus, id);
        assert_eq!(app.panes.iter().filter(|p| matches!(p.kind, PaneKind::Chart(_))).count(), 2);
        assert!(
            app.panes
                .iter()
                .filter_map(|p| if let PaneKind::Chart(c) = &p.kind { Some(c) } else { None })
                .all(|c| c.symbol == "MSFT US Equity" && c.archive)
        );
    }
    #[test]
    fn rotation_gaps_stop_tails_and_only_missing_archive_names_can_be_added() {
        let mut r = report();
        r.tracks[0].points[258] = None;
        assert!(current(&r.tracks[0], 258.5).unwrap().lagged);
        assert_eq!(current(&r.tracks[0], 258.5).unwrap().at, 257);
        r.skipped = vec![
            rotation::Skipped { symbol: "XLC".into(), reason: "not in archive".into() },
            rotation::Skipped { symbol: "SPY".into(), reason: "unverified".into() },
        ];
        let mut p = RotationPane::new("");
        p.accept(0, Ok(r));
        assert_eq!(archive_add(&p).as_deref(), Some("HIST ADD XLC"));
    }
    #[test]
    fn rotation_tail_mesh_splits_colours_at_axis_crossings() {
        let plot = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 400.0));
        let view = Rect::from_min_max(pos2(98.0, 98.0), pos2(102.0, 102.0));
        let mut mesh = Mesh::default();
        let a = Point { ratio: 99.0, momentum: 101.0 };
        let b = Point { ratio: 101.0, momentum: 99.0 };
        tail_segment(&mut mesh, a, b, 0.0, 1.0, 1.0, plot, view);
        assert_eq!(mesh.vertices.len(), 4);
        // Nearest 2×2 sampling changes texels at each interpolated axis crossing.
        for f in [0.0, 0.49, 0.50, 0.51, 1.0] {
            let uv = mesh.vertices[0].uv + (mesh.vertices[2].uv - mesh.vertices[0].uv) * f;
            let pixel = usize::from(uv.x >= 0.5) + 2 * usize::from(uv.y >= 0.5);
            let point = Point {
                ratio: a.ratio + (b.ratio - a.ratio) * f,
                momentum: a.momentum + (b.momentum - a.momentum) * f,
            };
            assert_eq!(
                [Quadrant::Lagging, Quadrant::Weakening, Quadrant::Improving, Quadrant::Leading][pixel],
                rotation::quadrant(point)
            );
        }
        mesh.clear();
        for _ in 0..40 {
            for _ in 0..30 {
                tail_segment(&mut mesh, a, b, 0.0, 1.0, 1.0, plot, view);
            }
        }
        assert!(mesh.vertices.len() <= 9_600);
    }
    #[test]
    fn rotation_stage_is_empty_when_loading_or_refused_and_idle_stops_repainting() {
        let ctx = egui::Context::default();
        let mut p = RotationPane::new("");
        p.error = Some("Rotation refused: no daily history. Sample/display prices are never used.".into());
        let frame = |p: &mut RotationPane, time: f64| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0))),
                    ..Default::default()
                },
                |ui| {
                    paint_stage(ui, p, 1, vec2(600.0, 400.0));
                },
            );
            output.textures_delta.clear();
            output
        };
        let out = frame(&mut p, 0.0);
        assert!(!out.shapes.iter().any(|s| matches!(s.shape, egui::Shape::Circle(_))));
        p.accept(0, Ok(report()));
        for i in 1..10 {
            frame(&mut p, i as f64).textures_delta.clear();
        }
        let out = frame(&mut p, 10.0);
        assert!(out.viewport_output.values().all(|v| v.repaint_delay > std::time::Duration::ZERO));
    }
}
