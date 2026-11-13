//! Seasonality Studio pane and painter charts.
use super::{PaneKind, TerminalApp};
use crate::{
    congress_view::Action,
    dock::PaneId,
    theme,
    worker::{Request, Worker},
};
use egui::{Align2, Color32, Rect, RichText, ScrollArea, Sense, Stroke, Ui, pos2, vec2};
use tr_data::studio_season::{self as math, Bucket, MIN_N, SeasonReport};

pub struct SeasonPane {
    pub symbol: String,
    pub spec: String,
    pub generation: u64,
    pub loading: bool,
    pub error: Option<String>,
    pub report: Option<SeasonReport>,
}

impl SeasonPane {
    pub fn new(symbol: impl Into<String>, spec: impl Into<String>) -> Self {
        Self { symbol: symbol.into(), spec: spec.into(), generation: 0, loading: false, error: None, report: None }
    }
    pub fn request(&mut self, id: PaneId, worker: &Worker) {
        self.generation += 1;
        self.report = None;
        self.loading = false;
        self.error = None;
        if self.symbol.trim().is_empty() {
            self.error = Some("SEASON needs a ticker, e.g. AAPL SEASON or AAPL SEASON 30M.".into());
            return;
        }
        let tf = match math::parse_timeframe(&self.spec) {
            Ok(tf) => tf,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        self.loading = true;
        worker.send(Request::StudioSeason {
            pane: id,
            generation: self.generation,
            symbol: tr_data::security(&self.symbol).key(),
            tf,
        });
    }
    pub fn accept(&mut self, generation: u64, result: Result<SeasonReport, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        self.report = None;
        match result {
            Ok(r) => match r.provenance.require_verified("Seasonality") {
                Ok(()) => {
                    self.error = None;
                    self.report = Some(r);
                }
                Err(e) => self.error = Some(e),
            },
            Err(e) => self.error = Some(e),
        }
    }
}

impl TerminalApp {
    pub(super) fn open_studio_season(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        self.status = "Seasonality Studio · verified calendar patterns".into();
        self.open_panel(PaneKind::StudioSeason(Box::new(SeasonPane::new(symbol, spec))));
    }
    pub(super) fn studio_season_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("season pane");
        let PaneKind::StudioSeason(pane) = &mut self.panes[i].kind else { return vec![] };
        let sample = self.provider.eq_ignore_ascii_case("sample");
        if sample {
            // Same empty state as DIST / CORR: never send a request for display prices.
            pane.report = None;
            pane.loading = false;
            pane.error = Some("No real data in sample / demo display mode. Seasonality never uses display or demo prices.".into());
        } else if pane.generation == 0 {
            pane.request(id, &self.worker);
        }
        let mut reload = false;
        ui.horizontal_wrapped(|ui| {
            super::studio_dist::studio_title(ui, "SEASONALITY STUDIO");
            if ui.small_button("Reload").clicked() {
                reload = true;
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Ticker");
            let edit = ui.add(egui::TextEdit::singleline(&mut pane.symbol).desired_width(100.0));
            reload |= edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            for (spec, label) in [("", "Daily"), ("5m", "5m"), ("15m", "15m"), ("30m", "30m"), ("1h", "1h")] {
                let selected = math::parse_timeframe(&pane.spec).ok() == math::parse_timeframe(spec).ok();
                if ui.selectable_label(selected, label).clicked() {
                    pane.spec = spec.into();
                    reload = true;
                }
            }
        });
        if reload && !sample {
            pane.request(id, &self.worker);
        }
        super::studio_dist::studio_states(ui, pane.loading, pane.error.as_deref(), None);
        if let Some(r) = &pane.report {
            ui.push_id(id, |ui| report_view(ui, id, r));
        }
        vec![]
    }
}

fn report_view(ui: &mut Ui, id: PaneId, r: &SeasonReport) {
    super::studio_dist::gap_warning(ui, &r.gaps);
    ui.label(
        RichText::new(format!(
            "{} · {} · {} {} bars · {} … {}",
            tr_data::security(&r.symbol).ticker,
            r.provenance.label(),
            r.bars,
            r.timeframe.label(),
            r.start,
            r.end
        ))
        .font(theme::mono(11.0))
        .color(theme::muted()),
    );
    ui.label(
        RichText::new(format!(
            "{} · observed {} UTC",
            r.source,
            chrono::DateTime::from_timestamp(r.observed_at, 0).unwrap_or_default().format("%Y-%m-%d %H:%M")
        ))
        .font(theme::mono(10.5))
        .color(theme::muted()),
    );
    ui.label(
        RichText::new(format!(
            "Grey = small sample (n < {MIN_N}) or incomplete. Calendar buckets use New York exchange dates. \
             Hover for counts and details. All returns are observed; no extrapolation."
        ))
        .small()
        .color(theme::muted()),
    );
    ScrollArea::vertical().id_salt((id, "season-scroll")).auto_shrink([false, false]).show(ui, |ui| {
        if r.timeframe.is_intraday() {
            intraday(ui, r);
        } else {
            daily(ui, r);
        }
        forwards(ui, r);
    });
}

fn pct(v: Option<f64>) -> String {
    v.map(|v| format!("{:+.2}%", v * 100.0)).unwrap_or_else(|| "—".into())
}
/// Hit rates are shares, not signed returns.
fn rate(v: Option<f64>) -> String {
    v.map(|v| format!("{:.1}%", v * 100.0)).unwrap_or_else(|| "—".into())
}
fn num(v: Option<f64>) -> String {
    v.map(|v| format!("{v:.2}")).unwrap_or_else(|| "—".into())
}
fn heading(ui: &mut Ui, label: &str) {
    super::studio_dist::studio_heading(ui, label);
}
fn color(v: f64) -> Color32 {
    if v >= 0.0 { theme::up() } else { theme::down() }
}
fn tint(v: Option<f64>, n: usize, scale: f64) -> Color32 {
    if n < MIN_N {
        return theme::control();
    }
    let Some(v) = v else { return theme::control() };
    theme::card().lerp_to_gamma(color(v), (0.18 + 0.65 * (v.abs() / scale.max(1e-9)).min(1.0)) as f32)
}
fn details(b: &Bucket) -> String {
    format!(
        "{} · n={}\nMean {} · median {}\nHit rate {} · t-stat {}\nSample standard deviation {}{}",
        b.label,
        b.stats.count,
        pct(b.stats.mean),
        pct(b.stats.median),
        rate(b.stats.win_rate),
        num(b.t_stat),
        b.stats.std.map(|s| format!("{:.2}%", s * 100.0)).unwrap_or_else(|| "—".into()),
        if b.stats.count < MIN_N { "\nSmall sample (n < 10); shown in grey." } else { "" }
    )
}

fn daily(ui: &mut Ui, r: &SeasonReport) {
    heading(ui, "YEAR × MONTH · actual compounded returns");
    ScrollArea::horizontal().id_salt("season-month-heat").show(ui, |ui| {
        let years: Vec<i32> = r.months.iter().map(|m| m.year).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        let width = ui.available_width().max(850.0);
        let cell_w = (width - 52.0) / 12.0;
        let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0 + years.len() as f32 * 44.0), Sense::hover());
        for (m, label) in math::MONTHS.iter().enumerate() {
            ui.painter().text(pos2(rect.left() + 52.0 + (m as f32 + 0.5) * cell_w, rect.top() + 10.0), Align2::CENTER_CENTER, label, theme::mono(11.0), theme::muted());
        }
        let scale = r.months.iter().filter_map(|m| m.value).map(f64::abs).fold(0.01, f64::max);
        for (y, year) in years.iter().enumerate() {
            let top = rect.top() + 24.0 + y as f32 * 44.0;
            ui.painter().text(pos2(rect.left(), top + 20.0), Align2::LEFT_CENTER, year, theme::mono(12.0), theme::text());
            for month in 1..=12 {
                let cell = r.months.iter().find(|m| m.year == *year && m.month == month);
                let (value, n) = cell.map(|c| (c.value, c.n)).unwrap_or((None, 0));
                let rc = Rect::from_min_size(pos2(rect.left() + 52.0 + (month - 1) as f32 * cell_w, top), vec2(cell_w - 3.0, 40.0));
                ui.painter().rect_filled(rc, 3.0, tint(value, n, scale));
                ui.painter().text(rc.center_top() + vec2(0.0, 12.0), Align2::CENTER_CENTER, pct(value), theme::mono(11.0), if value.is_some() && n >= MIN_N { theme::text() } else { theme::muted() });
                ui.painter().text(rc.center_bottom() - vec2(0.0, 9.0), Align2::CENTER_CENTER, format!("n={n}"), theme::mono(9.0), theme::muted());
                ui.interact(rc, ui.id().with(("month", year, month)), Sense::hover()).on_hover_text(match cell {
                    Some(c) => format!("{year}-{:02} · {}\n{} / {} daily returns\nRequires every session and prior month closing anchor.", month, pct(value), n, c.expected),
                    None => "No observations · n=0".into(),
                });
            }
        }
    });
    heading(ui, "MONTH OF YEAR · completed monthly returns");
    profile(ui, &r.month_profile, "Mean monthly return", false);
    hit_rates(ui, &r.month_profile, r.month_baseline.stats.win_rate);
    profile_table(ui, &r.month_profile, "month-table", Some(&r.month_baseline));
    heading(ui, "DAY OF WEEK · close-to-close daily returns");
    profile(ui, &r.weekdays, "Mean daily return", false);
    profile_table(ui, &r.weekdays, "weekday-table", Some(&r.baseline));
    heading(ui, "TURN OF MONTH · last session (−1) through 3rd session (+3) vs rest");
    profile(ui, &r.turn_of_month, "Mean daily return", false);
    profile_table(ui, &r.turn_of_month, "turn-table", Some(&r.baseline));
    heading(ui, "WEEK OF MONTH · calendar days 1–7, 8–14, 15–21, 22–28, 29–31");
    profile(ui, &r.weeks, "Mean daily return", false);
    profile_table(ui, &r.weeks, "week-table", Some(&r.baseline));
    ui.label(
        RichText::new(
            "Compare each bucket's hit rate and mean with the All row (every return in the sample), not with 50% / 0. \
             t-stat = mean / (sample sd / √n), one-period non-overlapping returns; many buckets are tested at once, \
             so |t| ≈ 2 is expected by chance somewhere.",
        )
        .small()
        .color(theme::muted()),
    );
}

/// Signed bar plot, with a shared zero axis and median markers.
fn profile(ui: &mut Ui, buckets: &[Bucket], axis: &str, dense: bool) {
    ScrollArea::horizontal().id_salt(axis).show(ui, |ui| {
        let width = ui.available_width().max(if dense { buckets.len() as f32 * 27.0 + 64.0 } else { 390.0 });
        let (outer, _) = ui.allocate_exact_size(vec2(width, 185.0), Sense::hover());
        let plot = Rect::from_min_max(outer.min + vec2(60.0, 14.0), outer.max - vec2(8.0, 31.0));
        let scale = buckets
            .iter()
            .filter(|b| b.stats.count >= MIN_N)
            .flat_map(|b| [b.stats.mean, b.stats.median])
            .flatten()
            .map(f64::abs)
            .fold(0.0001, f64::max)
            * 1.15;
        let y = |v: f64| plot.center().y - (v / scale) as f32 * plot.height() * 0.5;
        for step in -2..=2 {
            let v = scale * step as f64 / 2.0;
            ui.painter().line_segment(
                [pos2(plot.left(), y(v)), pos2(plot.right(), y(v))],
                Stroke::new(1.0, if step == 0 { theme::border() } else { theme::grid() }),
            );
            ui.painter().text(
                pos2(plot.left() - 5.0, y(v)),
                Align2::RIGHT_CENTER,
                format!("{:.2}%", v * 100.0),
                theme::mono(9.0),
                theme::muted(),
            );
        }
        let w = plot.width() / buckets.len().max(1) as f32;
        for (i, b) in buckets.iter().enumerate() {
            let x = plot.left() + i as f32 * w;
            if let Some(v) = b.stats.mean {
                let rc =
                    Rect::from_min_max(pos2(x + w * 0.2, y(v).min(y(0.0))), pos2(x + w * 0.8, y(v).max(y(0.0)) + 1.0));
                ui.painter().rect_filled(
                    rc.intersect(plot),
                    2.0,
                    if b.stats.count >= MIN_N { color(v) } else { theme::muted() },
                );
                if b.stats.count >= MIN_N
                    && let Some(m) = b.stats.median
                {
                    ui.painter().line_segment(
                        [pos2(x + w * 0.15, y(m)), pos2(x + w * 0.85, y(m))],
                        Stroke::new(2.0, theme::cyan()),
                    );
                }
            }
            let stride = if dense { (buckets.len() / 13).max(1) } else { 1 };
            if i % stride == 0 {
                ui.painter().text(
                    pos2(x + w * 0.5, plot.bottom() + 7.0),
                    Align2::CENTER_TOP,
                    &b.label,
                    theme::mono(9.0),
                    theme::muted(),
                );
            }
            ui.painter().text(
                pos2(x + w * 0.5, plot.top()),
                Align2::CENTER_TOP,
                format!("n={}", b.stats.count),
                theme::mono(8.0),
                theme::muted(),
            );
            ui.interact(
                Rect::from_min_max(pos2(x, plot.top()), pos2(x + w, plot.bottom())),
                ui.id().with((axis, i)),
                Sense::hover(),
            )
            .on_hover_text(details(b));
        }
    });
    ui.label(RichText::new(format!("{axis} · cyan marker = median")).small().color(theme::muted()));
}

fn hit_rates(ui: &mut Ui, buckets: &[Bucket], baseline: Option<f64>) {
    ui.label(
        RichText::new(format!(
            "Share of positive monthly returns · cyan line = all-months baseline {}",
            rate(baseline)
        ))
        .small()
        .color(theme::muted()),
    );
    ScrollArea::horizontal().id_salt("season-hit-rate").show(ui, |ui| {
        let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width().max(500.0), 125.0), Sense::hover());
        let plot = Rect::from_min_max(outer.min + vec2(45.0, 14.0), outer.max - vec2(8.0, 22.0));
        for v in [0.0, 0.5, 1.0] {
            let y = plot.bottom() - plot.height() * v;
            ui.painter().line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, theme::grid()));
            ui.painter().text(
                pos2(plot.left() - 4.0, y),
                Align2::RIGHT_CENTER,
                format!("{:.0}%", v * 100.0),
                theme::mono(9.0),
                theme::muted(),
            );
        }
        if let Some(v) = baseline {
            let y = plot.bottom() - plot.height() * v as f32;
            ui.painter().line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.5, theme::cyan()));
        }
        let w = plot.width() / buckets.len().max(1) as f32;
        for (i, b) in buckets.iter().enumerate() {
            let x = plot.left() + (i as f32 + 0.5) * w;
            if let Some(rate) = b.stats.win_rate {
                let top = plot.bottom() - rate as f32 * plot.height();
                ui.painter().rect_filled(
                    Rect::from_min_max(pos2(x - w * 0.3, top), pos2(x + w * 0.3, plot.bottom())),
                    2.0,
                    if b.stats.count >= MIN_N { theme::amber() } else { theme::muted() },
                );
            }
            ui.painter().text(
                pos2(x, plot.bottom() + 5.0),
                Align2::CENTER_TOP,
                &b.label,
                theme::mono(9.0),
                theme::muted(),
            );
            ui.painter().text(
                pos2(x, outer.top()),
                Align2::CENTER_TOP,
                format!("n={}", b.stats.count),
                theme::mono(8.0),
                theme::muted(),
            );
            ui.interact(
                Rect::from_min_max(pos2(x - w / 2.0, plot.top()), pos2(x + w / 2.0, plot.bottom())),
                ui.id().with(("hit", i)),
                Sense::hover(),
            )
            .on_hover_text(details(b));
        }
    });
}

fn profile_table(ui: &mut Ui, buckets: &[Bucket], id: &str, baseline: Option<&Bucket>) {
    ScrollArea::horizontal().id_salt(id).show(ui, |ui| {
        egui::Grid::new(id).striped(true).spacing(vec2(14.0, 4.0)).show(ui, |ui| {
            for s in ["Bucket", "Mean", "Median", "Hit rate", "t-stat", "n"] {
                ui.label(RichText::new(s).font(theme::mono(10.0)).color(theme::muted()));
            }
            ui.end_row();
            for (i, b) in buckets.iter().chain(baseline).enumerate() {
                let is_baseline = i == buckets.len();
                let c = if is_baseline {
                    theme::cyan()
                } else if b.stats.count >= MIN_N {
                    theme::text()
                } else {
                    theme::muted()
                };
                for s in [
                    b.label.clone(),
                    pct(b.stats.mean),
                    pct(b.stats.median),
                    rate(b.stats.win_rate),
                    num(b.t_stat),
                    b.stats.count.to_string(),
                ] {
                    ui.label(RichText::new(s).font(theme::mono(11.0)).color(c));
                }
                ui.end_row();
            }
        });
    });
}

fn intraday(ui: &mut Ui, r: &SeasonReport) {
    heading(ui, "NEW YORK CLOCK · regular session, closed bars only");
    ui.label(RichText::new("Bar open → close returns exclude overnight gaps. Half days follow the exchange calendar. Volume shares use complete sessions only.").small().color(theme::muted()));
    let returns: Vec<_> = r.clock.iter().map(|c| c.returns.clone()).collect();
    let absolute: Vec<_> = r.clock.iter().map(|c| c.absolute.clone()).collect();
    let volume: Vec<_> = r.clock.iter().map(|c| c.volume_share.clone()).collect();
    profile(ui, &returns, "Mean bar return", true);
    profile(ui, &absolute, "Mean absolute bar return · volatility smile", true);
    profile(ui, &volume, "Mean share of session volume", true);
    heading(ui, "DAY OF WEEK × NEW YORK CLOCK · mean bar return");
    ScrollArea::horizontal().id_salt("season-clock-heat").show(ui, |ui| {
        let width = ui.available_width().max(r.clock.len() as f32 * 55.0 + 42.0);
        let cell_w = (width - 42.0) / r.clock.len().max(1) as f32;
        let (rect, _) = ui.allocate_exact_size(vec2(width, 235.0), Sense::hover());
        let scale = r
            .clock
            .iter()
            .flat_map(|c| &c.weekdays)
            .filter(|b| b.stats.count >= MIN_N)
            .filter_map(|b| b.stats.mean)
            .map(f64::abs)
            .fold(0.0001, f64::max);
        for (col, c) in r.clock.iter().enumerate() {
            let x = rect.left() + 42.0 + col as f32 * cell_w;
            ui.painter().text(
                pos2(x + cell_w / 2.0, rect.top() + 10.0),
                Align2::CENTER_CENTER,
                &c.returns.label,
                theme::mono(9.0),
                theme::muted(),
            );
            for (day, b) in c.weekdays.iter().enumerate() {
                let rc = Rect::from_min_size(pos2(x, rect.top() + 25.0 + day as f32 * 41.0), vec2(cell_w - 2.0, 38.0));
                ui.painter().rect_filled(rc, 3.0, tint(b.stats.mean, b.stats.count, scale));
                ui.painter().text(
                    rc.center() - vec2(0.0, 6.0),
                    Align2::CENTER_CENTER,
                    pct(b.stats.mean),
                    theme::mono(9.0),
                    if b.stats.count >= MIN_N { theme::text() } else { theme::muted() },
                );
                ui.painter().text(
                    rc.center() + vec2(0.0, 9.0),
                    Align2::CENTER_CENTER,
                    format!("n={}", b.stats.count),
                    theme::mono(8.0),
                    theme::muted(),
                );
                ui.interact(rc, ui.id().with(("clock", col, day)), Sense::hover()).on_hover_text(format!(
                    "{} NY\n{}",
                    c.returns.label,
                    details(b)
                ));
            }
        }
        for (day, label) in math::WEEKDAYS.iter().enumerate() {
            ui.painter().text(
                pos2(rect.left(), rect.top() + 44.0 + day as f32 * 41.0),
                Align2::LEFT_CENTER,
                label,
                theme::mono(10.0),
                theme::text(),
            );
        }
    });
}

fn forwards(ui: &mut Ui, r: &SeasonReport) {
    heading(ui, "WHAT FOLLOWED · current calendar buckets vs every completed outcome");
    let month = math::MONTHS[r.current_month as usize - 1];
    let weekday = math::WEEKDAYS.get(r.current_weekday).copied().unwrap_or("Weekend");
    ui.label(RichText::new(format!("Current NY month {month} / weekday {weekday}. Forward returns are close-to-close over 5/10/20 observed bars, including overnight gaps. Missing sessions and vault gaps invalidate crossing windows. Overlapping windows share future bars, so naive iid t-statistics would overstate significance; the t-stat column uses only non-overlapping windows (each starting ≥ H bars after the previous one).")).small().color(theme::muted()));
    for f in &r.forwards {
        ui.push_id(f.horizon, |ui| {
            heading(ui, &format!("{} bars forward", f.horizon));
            let bs = [
                Bucket { label: month.into(), stats: f.month.clone(), t_stat: f.month_independent.t_stat },
                Bucket { label: weekday.into(), stats: f.weekday.clone(), t_stat: f.weekday_independent.t_stat },
                Bucket { label: "Baseline".into(), stats: f.baseline.clone(), t_stat: f.baseline_independent.t_stat },
            ];
            profile_table(ui, &bs, "forward-table", None);
            ui.label(
                RichText::new(format!(
                    "t-stat on non-overlapping {h}-bar windows only · independent n: month {} · weekday {} · baseline {}. \
                     Mean, median and hit rate use every (overlapping) window.",
                    f.month_independent.n,
                    f.weekday_independent.n,
                    f.baseline_independent.n,
                    h = f.horizon
                ))
                .small()
                .color(theme::muted()),
            );
            distribution(
                ui,
                &[
                    (&f.baseline_values, "Baseline", theme::muted()),
                    (&f.month_values, month, theme::amber()),
                    (&f.weekday_values, weekday, theme::cyan()),
                ],
            );
        });
    }
}

fn distribution(ui: &mut Ui, groups: &[(&Vec<f64>, &str, Color32)]) {
    const BINS: usize = 24;
    let all = groups.iter().flat_map(|(v, _, _)| v.iter()).copied().collect::<Vec<_>>();
    if all.is_empty() {
        ui.label("No completed outcomes · n=0");
        return;
    }
    let mut lo = all.iter().copied().fold(f64::INFINITY, f64::min);
    let mut hi = all.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if hi - lo < 1e-9 {
        lo -= 0.001;
        hi += 0.001;
    }
    let step = (hi - lo) / BINS as f64;
    let hist: Vec<Vec<usize>> = groups
        .iter()
        .map(|(values, _, _)| {
            let mut counts = vec![0; BINS];
            for v in *values {
                let i = (((v - lo) / step).floor() as usize).min(BINS - 1);
                counts[i] += 1;
            }
            counts
        })
        .collect();
    let max = hist
        .iter()
        .zip(groups)
        .filter(|(_, (v, _, _))| v.len() >= MIN_N)
        .flat_map(|(counts, (v, _, _))| counts.iter().map(move |n| *n as f64 / v.len() as f64))
        .fold(0.01, f64::max);
    let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width().max(220.0), 170.0), Sense::hover());
    let plot = Rect::from_min_max(outer.min + vec2(48.0, 8.0), outer.max - vec2(8.0, 27.0));
    for i in 0..=4 {
        let y = plot.bottom() - plot.height() * i as f32 / 4.0;
        ui.painter().line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, theme::grid()));
        ui.painter().text(
            pos2(plot.left() - 3.0, y),
            Align2::RIGHT_CENTER,
            format!("{:.0}%", max * i as f64 * 25.0),
            theme::mono(9.0),
            theme::muted(),
        );
    }
    let w = plot.width() / BINS as f32;
    for (g, (values, _, c)) in groups.iter().enumerate() {
        let mut points = Vec::new();
        for (i, count) in hist[g].iter().enumerate() {
            let share = *count as f64 / values.len().max(1) as f64;
            let x = plot.left() + (i as f32 + 0.5) * w;
            let y = plot.bottom() - (share / max) as f32 * plot.height();
            points.push(pos2(x, y.clamp(plot.top(), plot.bottom())));
        }
        if values.len() >= MIN_N {
            ui.painter().add(egui::Shape::line(points, Stroke::new(if g == 0 { 1.0 } else { 2.0 }, *c)));
        }
    }
    for (i, _) in hist[0].iter().enumerate() {
        let mut tip = format!("{} … {}", pct(Some(lo + i as f64 * step)), pct(Some(lo + (i + 1) as f64 * step)));
        for (g, (values, label, _)) in groups.iter().enumerate() {
            tip.push_str(&format!("\n{label}: bin n={} / total n={}", hist[g][i], values.len()));
        }
        ui.interact(
            Rect::from_min_size(pos2(plot.left() + i as f32 * w, plot.top()), vec2(w, plot.height())),
            ui.id().with(("hist", i)),
            Sense::hover(),
        )
        .on_hover_text(tip);
    }
    for (x, v) in [(plot.left(), lo), (plot.center().x, (lo + hi) / 2.0), (plot.right(), hi)] {
        ui.painter().text(
            pos2(x, plot.bottom() + 5.0),
            Align2::CENTER_TOP,
            pct(Some(v)),
            theme::mono(9.0),
            theme::muted(),
        );
    }
    ui.horizontal_wrapped(|ui| {
        for (v, label, c) in groups {
            ui.colored_label(if v.len() >= MIN_N { *c } else { theme::muted() }, format!("{label} · n={}", v.len()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layouts::PaneSettings;
    #[test]
    fn charts_render_sparse_and_constant_data_at_dock_and_desktop_widths() {
        let mut bars = Vec::new();
        for date in ["2026-11-09", "2026-11-10", "2026-11-11"] {
            for ts in tr_data::intraday::regular_opens(date.parse().unwrap(), tr_core::Timeframe::M30) {
                bars.push(
                    tr_core::Bar::new(ts, 100.0, 100.0, 100.0, 100.0, 10.0).with_provenance(tr_core::Provenance::Real),
                );
            }
        }
        let now = bars.last().unwrap().ts + 1800;
        let intraday_report = math::compute("X", tr_core::Timeframe::M30, &bars, "unit", now).unwrap();
        let daily_bars: Vec<_> = ["2026-11-09", "2026-11-10", "2026-11-11"]
            .iter()
            .map(|s| {
                let ts = s.parse::<chrono::NaiveDate>().unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
                tr_core::Bar::new(ts, 100.0, 100.0, 100.0, 100.0, 10.0).with_provenance(tr_core::Provenance::Real)
            })
            .collect();
        let daily_report = math::compute("X", tr_core::Timeframe::D1, &daily_bars, "unit", now + 3600).unwrap();
        for width in [280.0, 1440.0] {
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 1440.0))),
                    ..Default::default()
                },
                |ui| {
                    ScrollArea::vertical().show(ui, |ui| {
                        daily(ui, &daily_report);
                        intraday(ui, &intraday_report);
                        forwards(ui, &intraday_report);
                    });
                },
            );
            output.textures_delta.clear();
            assert!(!output.shapes.is_empty());
        }
    }
    #[test]
    fn view_warns_when_series_spans_vault_gap() {
        let mut bars = Vec::new();
        for date in ["2026-11-09", "2026-11-12"] {
            for ts in tr_data::intraday::regular_opens(date.parse().unwrap(), tr_core::Timeframe::M30) {
                bars.push(
                    tr_core::Bar::new(ts, 100.0, 101.0, 99.0, 100.5, 10.0).with_provenance(tr_core::Provenance::Real),
                );
            }
        }
        let gap = (bars[12].ts, bars[13].ts);
        let now = bars.last().unwrap().ts + 1800;
        let r = math::compute_with_gaps("X", tr_core::Timeframe::M30, &bars, "unit", now, now, &[gap]).unwrap();
        assert_eq!(r.gaps, vec![gap]);
        // No forward window crosses the gap: only same-session windows remain.
        assert_eq!(r.forwards[0].baseline.count, 2 * (13 - 5));
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| report_view(ui, 1, &r));
        output.textures_delta.clear();
        assert!(format!("{:?}", output.shapes).contains("UNVERIFIED JOIN"));
    }
    #[test]
    fn season_command_and_new_panel_open_studio() {
        let mut app = crate::app::tests::test_app();
        app.apply(crate::command::Function::StudioSeason { spec: "30m".into() });
        assert!(app.panes.iter().any(|p| matches!(p.kind, PaneKind::StudioSeason(_))));
        assert!(matches!(app.panel_from_spec("SEASON 30M", Some("AAPL")), Some(PaneKind::StudioSeason(_))));
    }
    #[test]
    fn season_settings_round_trip_through_kind_and_json() {
        let settings = PaneSettings::StudioSeason { symbol: "AAPL US Equity".into(), spec: "30m".into() };
        let json = serde_json::to_vec(&settings).unwrap();
        let kind = PaneKind::from_settings(serde_json::from_slice(&json).unwrap());
        assert_eq!(kind.code(), "SEASON");
        assert!(kind.tab_title().contains("AAPL"));
        assert_eq!(serde_json::to_value(kind.settings()).unwrap(), serde_json::to_value(settings).unwrap());
    }
    #[test]
    fn stale_generation_and_unverified_reports_are_rejected() {
        let mut p = SeasonPane::new("AAPL", "");
        p.generation = 2;
        p.loading = true;
        p.accept(1, Err("old response".into()));
        assert!(p.loading);
        assert!(p.error.is_none());
        p.accept(2, Err("no verified history".into()));
        assert!(!p.loading);
        assert!(p.report.is_none());
        let opens = tr_data::intraday::regular_opens("2026-11-12".parse().unwrap(), tr_core::Timeframe::M30);
        let bars: Vec<_> = opens
            .iter()
            .map(|ts| {
                tr_core::Bar::new(*ts, 100.0, 100.0, 100.0, 100.0, 10.0).with_provenance(tr_core::Provenance::Real)
            })
            .collect();
        let mut report =
            math::compute("X", tr_core::Timeframe::M30, &bars, "unit", opens.last().unwrap() + 1800).unwrap();
        report.provenance = tr_core::Provenance::Sample;
        p.accept(2, Ok(report));
        assert!(p.error.as_deref().unwrap().contains("SAMPLE"));
        assert!(p.report.is_none());
    }
}
