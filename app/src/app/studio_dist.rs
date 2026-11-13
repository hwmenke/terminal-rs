//! Distribution Studio pane and worker-side report assembly. All price loading is off the UI thread.
use super::{PaneKind, TerminalApp};
use crate::theme::{self, dp, mono, muted, text};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{Align2, Color32, Painter, Pos2, Rect, Response, RichText, Sense, Stroke, Ui, pos2, vec2};
use tr_core::{Bar, Timeframe};
use tr_data::studio_dist::{self as dist, Report, Window};

pub struct DistPane {
    pub symbol: String,
    pub timeframe: Timeframe,
    pub window: Window,
    pub generation: u64,
    pub report: Option<Report>,
    pub loading: bool,
    pub error: Option<String>,
    best_k: usize,
}
impl DistPane {
    pub fn new(symbol: impl Into<String>, timeframe: Timeframe, window: Window) -> Self {
        Self {
            symbol: symbol.into(),
            timeframe,
            window,
            generation: 0,
            report: None,
            loading: false,
            error: None,
            best_k: 5,
        }
    }
    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        self.generation += 1;
        self.report = None;
        self.error = None;
        self.loading = false;
        if self.symbol.trim().is_empty() {
            self.error = Some("DIST needs a ticker (AAPL DIST). Only verified real bars can be used.".into());
            return;
        }
        self.loading = true;
        worker.send(Request::StudioDist {
            pane,
            generation: self.generation,
            symbol: tr_data::security(&self.symbol).key(),
            timeframe: self.timeframe,
            window: self.window,
        });
    }
    fn deny_sample(&mut self) {
        if self.loading || self.report.is_some() {
            self.generation += 1;
        }
        self.report = None;
        self.loading = false;
        self.error =
            Some("Sample / demo mode: no real data. Distribution Studio never computes from display prices.".into());
    }
    pub fn accept(&mut self, generation: u64, result: Result<Report, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        self.report = None;
        match result {
            Ok(report) => match report.provenance.require_verified("Distribution Studio") {
                Ok(()) => {
                    self.report = Some(report);
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            },
            Err(e) => self.error = Some(e),
        }
    }
}

/// Same archive-first policy as STATS. Called only by the background worker.
pub(crate) async fn daily_bars(
    history: &tr_data::history::Service,
    bridge: &tr_data::whats_news::Bridge,
    key: &str,
) -> anyhow::Result<(Vec<Bar>, String)> {
    let ticker = tr_data::whats_news::archive_ticker(key);
    if ticker.is_empty() {
        anyhow::bail!("Distribution Studio refused: invalid security");
    }
    let wanted = ticker.clone();
    let local = history.db(move |db| db.daily(&wanted)).await?;
    if !local.is_empty() {
        tr_data::stats::require_research_bars(&local, "Distribution Studio").map_err(anyhow::Error::msg)?;
        return Ok((local, "Local Yahoo daily research archive".into()));
    }
    if !bridge.enabled {
        anyhow::bail!(
            "Distribution Studio: no verified daily history for {ticker} in the research archive. Sample/display prices are never used."
        );
    }
    let external = bridge.histories_for(std::slice::from_ref(&ticker)).await?;
    external.provenance.require_verified("Distribution Studio archive").map_err(anyhow::Error::msg)?;
    if let Some(skip) = external.data.skipped.iter().find(|s| s.symbol.eq_ignore_ascii_case(&ticker)) {
        anyhow::bail!("Distribution Studio refused: {}: {}", skip.symbol, skip.reason);
    }
    let bars = external
        .data
        .bars
        .into_iter()
        .find(|(s, _)| s.eq_ignore_ascii_case(&ticker))
        .map(|(_, b)| b)
        .unwrap_or_default();
    tr_data::stats::require_research_bars(&bars, "Distribution Studio").map_err(anyhow::Error::msg)?;
    Ok((bars, external.source))
}

pub(crate) async fn build_report(
    symbol: String,
    bars: Vec<Bar>,
    timeframe: Timeframe,
    window: Window,
    source: String,
    observed_at: Option<i64>,
) -> Result<Report, String> {
    tokio::task::spawn_blocking(move || {
        // Verify even the pending bar: contaminated inputs must never be silently trimmed away.
        tr_data::stats::require_research_bars(&bars, "Distribution Studio")?;
        let closed = tr_data::closed::closed_len(timeframe, &bars, tr_data::security(&symbol).class, observed_at);
        dist::compute(&symbol, &bars[..closed], timeframe, window, &source, observed_at)
    })
    .await
    .map_err(|e| format!("Distribution Studio worker: {e}"))?
}

impl TerminalApp {
    pub(super) fn open_studio_dist(&mut self, symbol: Option<&str>, timeframe: Timeframe) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        self.status = "Distribution Studio: log returns, tails, drawdowns and volatility regimes".into();
        self.open_panel(PaneKind::StudioDist(Box::new(DistPane::new(symbol, timeframe, Window::All))));
    }
    pub(super) fn studio_dist_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("distribution pane");
        let PaneKind::StudioDist(pane) = &mut self.panes[i].kind else { return vec![] };
        let sample = self.provider.eq_ignore_ascii_case("sample");
        if sample {
            pane.deny_sample();
        }
        if !sample && !pane.loading && pane.report.is_none() && pane.error.is_none() {
            pane.request(id, &self.worker);
        }
        let mut reload = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("DISTRIBUTION STUDIO").font(mono(14.0)).strong().color(theme::cyan()));
            reload |= ui.small_button("Refresh").clicked();
        });
        ui.horizontal_wrapped(|ui| {
            let input = ui.add(egui::TextEdit::singleline(&mut pane.symbol).desired_width(dp(105.0)).hint_text("AAPL"));
            reload |= input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            egui::ComboBox::from_id_salt((id, "dist-tf"))
                .selected_text(pane.timeframe.label())
                .width(dp(60.0))
                .show_ui(ui, |ui| {
                    for tf in [
                        Timeframe::D1,
                        Timeframe::M1,
                        Timeframe::M5,
                        Timeframe::M15,
                        Timeframe::M30,
                        Timeframe::H1,
                        Timeframe::H4,
                    ] {
                        reload |= ui.selectable_value(&mut pane.timeframe, tf, tf.label()).changed();
                    }
                });
            for window in Window::ALL {
                reload |= ui.selectable_value(&mut pane.window, window, window.label()).changed();
            }
        });
        if reload && !sample {
            pane.request(id, &self.worker);
        }
        if pane.loading {
            ui.label(RichText::new("Reading verified closed bars…").color(muted()));
        }
        if let Some(e) = &pane.error {
            ui.label(RichText::new(e).color(theme::down()));
        }
        if let Some(r) = &pane.report {
            ui.label(
                RichText::new(format!(
                    "{} · {} · {} closed bars / {} returns · {} to {}",
                    r.provenance.label(),
                    r.timeframe.label(),
                    r.bars,
                    r.moments.n,
                    date(r.start),
                    date(r.end)
                ))
                .font(mono(11.0))
                .color(muted()),
            );
            ui.label(RichText::new(&r.source).font(mono(10.5)).color(muted()));
            ui.label(
                RichText::new(format!(
                    "{}: {} · {} shows available coverage; missing bars are never filled.",
                    if r.timeframe == Timeframe::D1 { "Archive read / closure clock" } else { "Feed observation" },
                    r.observed_at.map(stamp).unwrap_or_else(|| "unavailable; final bar withheld".into()),
                    r.window.label()
                ))
                .size(11.0)
                .color(muted()),
            );
            egui::ScrollArea::vertical().id_salt((id,"dist-scroll")).auto_shrink([false,false]).show(ui,|ui| {
                metric_cards(ui,r);
                ui.add_space(dp(6.0));
                if ui.available_width()>dp(800.0) {
                    ui.columns(2,|columns| { histogram(&mut columns[0],r); qq(&mut columns[1],r); });
                } else { histogram(ui,r); qq(ui,r); }
                tail_rates(ui,r);
                underwater(ui,r);
                volatility(ui,r);
                drawdown_table(ui,r);
                regime_table(ui,r);
                ui.horizontal_wrapped(|ui| {
                    heading(ui,"BEST / WORST CLOSED BAR RETURNS");
                    ui.add(egui::DragValue::new(&mut pane.best_k).range(1..=dist::EXTREMES).prefix("k = "));
                });
                extremes(ui,r,pane.best_k);
                ui.label(RichText::new("Descriptive history only. Log-return tails and fitted curves are not forecasts. Overnight gaps remain in intraday close-to-close returns.").size(11.0).color(muted()));
            });
        }
        vec![]
    }
}
fn date(ts: i64) -> String {
    tr_data::stats::bar_date(ts)
}
fn stamp(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "—".into())
}
fn pct(x: f64) -> String {
    format!("{:.2}%", x * 100.0)
}
fn number(x: Option<f64>) -> String {
    x.map(|v| format!("{v:.3}")).unwrap_or_else(|| "—".into())
}
fn heading(ui: &mut Ui, s: &str) {
    ui.add_space(dp(8.0));
    ui.label(RichText::new(s).font(mono(11.0)).strong().color(muted()));
}
fn regime_color(i: usize) -> Color32 {
    [theme::cyan(), theme::amber(), theme::down()][i]
}
const REGIMES: [&str; 3] = ["Calm", "Normal", "Stressed"];
fn metric_cards(ui: &mut Ui, r: &Report) {
    let m = &r.moments;
    ui.horizontal_wrapped(|ui| {
        for (label, value) in [
            ("MEAN / BAR", pct(m.mean)),
            ("STDEV / ANN.", pct(m.annual_vol)),
            ("SKEW", number(m.skew)),
            ("EXCESS KURT.", number(m.excess_kurtosis)),
            ("JARQUE–BERA", number(m.jarque_bera)),
            ("JB P-VALUE", number(m.jb_p_value)),
            ("VaR 95 / 99", format!("{} / {}", pct(m.var95), pct(m.var99))),
            ("CVaR 95 / 99", format!("{} / {}", pct(m.cvar95), pct(m.cvar99))),
            ("TAIL RATIO", number(m.tail_ratio)),
        ] {
            egui::Frame::new().fill(theme::card()).inner_margin(dp(8.0)).show(ui, |ui| {
                ui.set_min_width(dp(112.0));
                ui.label(RichText::new(label).font(mono(10.0)).color(muted()));
                ui.label(RichText::new(value).font(mono(15.0)).color(theme::cyan()));
            });
        }
    });
    ui.label(RichText::new(format!("Volatility: sample σ × √{:.0} periods/year. VaR/CVaR: signed log-loss; negative values mean gains. JB p uses asymptotic χ²(2).",r.annual_periods)).size(11.0).color(muted()));
}

/// Shared painter axes. Coordinates stay in f64 until mapped into the viewport.
struct Plot {
    painter: Painter,
    rect: Rect,
    response: Response,
    x: [f64; 2],
    y: [f64; 2],
}
impl Plot {
    fn new(
        ui: &mut Ui,
        height: f32,
        x: [f64; 2],
        y: [f64; 2],
        x_label: impl Fn(f64) -> String,
        y_label: impl Fn(f64) -> String,
    ) -> Self {
        let (outer, response) =
            ui.allocate_exact_size(vec2(ui.available_width().max(dp(120.0)), dp(height)), Sense::hover());
        let rect = Rect::from_min_max(outer.min + vec2(dp(48.0), dp(12.0)), outer.max - vec2(dp(12.0), dp(26.0)));
        let painter = ui.painter_at(outer);
        painter.rect_filled(rect, 0.0, theme::card());
        let p = Self { painter, rect, response, x, y };
        for i in 0..=4 {
            let f = i as f64 / 4.0;
            let x = x[0] + f * (x[1] - x[0]);
            let y = y[0] + f * (y[1] - y[0]);
            let px = p.pos(x, p.y[0]).x;
            let py = p.pos(p.x[0], y).y;
            p.painter.line_segment([pos2(px, rect.top()), pos2(px, rect.bottom())], Stroke::new(0.5, theme::grid()));
            p.painter.line_segment([pos2(rect.left(), py), pos2(rect.right(), py)], Stroke::new(0.5, theme::grid()));
            // Narrow panes retain three x ticks to keep the labels legible.
            if rect.width() > dp(430.0) || i % 2 == 0 {
                p.painter.text(pos2(px, rect.bottom() + dp(6.0)), Align2::CENTER_TOP, x_label(x), mono(9.0), muted());
            }
            p.painter.text(pos2(rect.left() - dp(5.0), py), Align2::RIGHT_CENTER, y_label(y), mono(9.0), muted());
        }
        p.painter.line_segment([rect.left_top(), rect.left_bottom()], Stroke::new(1.0, muted()));
        p.painter.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, muted()));
        p
    }
    fn pos(&self, x: f64, y: f64) -> Pos2 {
        pos2(
            self.rect.left() + ((x - self.x[0]) / (self.x[1] - self.x[0])) as f32 * self.rect.width(),
            self.rect.bottom() - ((y - self.y[0]) / (self.y[1] - self.y[0])) as f32 * self.rect.height(),
        )
    }
    fn line(&self, points: impl Iterator<Item = (f64, f64)>, color: Color32) {
        let points: Vec<_> = points.map(|(x, y)| self.pos(x, y)).collect();
        // Preserve each pixel group's extrema while bounding tessellation on deep vault histories.
        let bucket = points.len().div_ceil(self.rect.width().max(1.0) as usize).max(1);
        let points = if bucket > 4 {
            let mut reduced = Vec::new();
            for chunk in points.chunks(bucket) {
                let lo = chunk.iter().enumerate().min_by(|(_, a), (_, b)| a.y.total_cmp(&b.y)).unwrap().0;
                let hi = chunk.iter().enumerate().max_by(|(_, a), (_, b)| a.y.total_cmp(&b.y)).unwrap().0;
                let mut indices = [0, lo, hi, chunk.len() - 1];
                indices.sort_unstable();
                for i in indices {
                    reduced.push(chunk[i]);
                }
            }
            reduced
        } else {
            points
        };
        if points.len() > 1 {
            self.painter.with_clip_rect(self.rect).add(egui::Shape::line(points, Stroke::new(dp(1.6), color)));
        }
    }
    fn hover_x(&self) -> Option<f64> {
        self.response
            .hover_pos()
            .filter(|p| self.rect.contains(*p))
            .map(|p| self.x[0] + ((p.x - self.rect.left()) / self.rect.width()) as f64 * (self.x[1] - self.x[0]))
    }
}
fn histogram(ui: &mut Ui, r: &Report) {
    heading(ui, "LOG RETURNS · OBSERVED COUNTS + FITTED DENSITIES");
    let bins = &r.histogram;
    let ymax = bins
        .iter()
        .map(|b| b.count as f64)
        .chain(r.normal_curve.iter().map(|(_, y)| *y))
        .chain(r.student_curve.iter().map(|(_, y)| *y))
        .fold(1.0, f64::max)
        * 1.12;
    let p = Plot::new(ui, 230.0, [bins[0].left, bins.last().unwrap().right], [0.0, ymax], pct, |v| format!("{v:.0}"));
    for b in bins {
        let left = p.pos(b.left, b.count as f64);
        let right = p.pos(b.right, 0.0);
        p.painter.with_clip_rect(p.rect).rect_filled(
            Rect::from_min_max(left + vec2(0.5, 0.0), right - vec2(0.5, 0.0)),
            0.0,
            theme::cyan().gamma_multiply(0.7),
        );
    }
    p.line(r.normal_curve.iter().copied(), theme::amber());
    p.line(r.student_curve.iter().copied(), theme::down());
    if let Some(x) = p.hover_x() {
        let bin = bins.iter().find(|b| x >= b.left && x <= b.right).unwrap_or(&bins[0]);
        p.response.on_hover_ui(|ui| {
            ui.label(format!("{} … {}\n{} observed returns", pct(bin.left), pct(bin.right), bin.count));
        });
    }
    ui.horizontal_wrapped(|ui| {
        ui.colored_label(theme::cyan(), "■ Observed");
        ui.colored_label(theme::amber(), "━ Normal");
        ui.colored_label(theme::down(), format!("━ Student-t (MoM df {})", number(r.moments.student_df)));
    });
    if r.moments.sigma == 0.0 {
        ui.label(RichText::new("Zero variance: density fits and QQ coordinates are undefined.").color(muted()));
    } else if r.moments.student_df.is_none() {
        ui.label(RichText::new("Student-t unavailable: excess kurtosis must be positive.").size(11.0).color(muted()));
    }
}
fn qq(ui: &mut Ui, r: &Report) {
    heading(ui, "NORMAL QQ · STANDARDIZED LOG RETURNS");
    if r.qq.is_empty() {
        ui.label(RichText::new("QQ unavailable for zero variance.").color(muted()));
        return;
    }
    let extent = r.qq.iter().flat_map(|p| [p.theoretical.abs(), p.observed.abs()]).fold(1.0, f64::max) * 1.08;
    let p = Plot::new(ui, 230.0, [-extent, extent], [-extent, extent], |x| format!("{x:.1}"), |y| format!("{y:.1}"));
    p.line([(-extent, -extent), (extent, extent)].into_iter(), theme::amber());
    let clipped = p.painter.with_clip_rect(p.rect);
    let stride = r.qq.len().div_ceil(2000).max(1);
    // Plot observed ranks only; hover still searches all ranks. Always retain both endpoints.
    for (i, q) in r.qq.iter().enumerate() {
        if i % stride != 0 && i + 1 != r.qq.len() {
            continue;
        }
        clipped.circle_filled(
            p.pos(q.theoretical, q.observed),
            dp(if q.tail { 2.6 } else { 1.5 }),
            if q.tail { theme::down() } else { theme::cyan() },
        );
    }
    if let Some(x) = p.hover_x() {
        let i = r.qq.partition_point(|q| q.theoretical < x).min(r.qq.len() - 1);
        let q = if i > 0 && (r.qq[i - 1].theoretical - x).abs() < (r.qq[i].theoretical - x).abs() {
            &r.qq[i - 1]
        } else {
            &r.qq[i]
        };
        p.response.on_hover_ui(|ui| {
            ui.label(format!(
                "Normal quantile {:.3}σ\nObserved {:.3}σ{}",
                q.theoretical,
                q.observed,
                if q.tail { " · outer 2.5% tail" } else { "" }
            ));
        });
    }
    if stride > 1 {
        ui.label(
            RichText::new("Displayed ranks sampled for clarity; hover includes every observation.")
                .size(11.0)
                .color(muted()),
        );
    }
    ui.label(
        RichText::new("x: normal quantile · y: observed z · amber: 45° · red: outer 2.5% tails")
            .size(11.0)
            .color(muted()),
    );
}
fn tail_rates(ui: &mut Ui, r: &Report) {
    for (k, shares, normal) in [(2, r.moments.beyond2, 4.5500264), (3, r.moments.beyond3, 0.2699796)] {
        ui.label(
            RichText::new(match shares {
                Some([lo, hi]) => format!(
                    "Beyond ±{k}σ: {} total (left {}, right {}) · normal {:.3}% total",
                    pct(lo + hi),
                    pct(lo),
                    pct(hi),
                    normal
                ),
                None => format!("Beyond ±{k}σ: unavailable (zero variance) · normal {normal:.3}% total"),
            })
            .font(mono(11.0))
            .color(muted()),
        );
    }
}
fn underwater(ui: &mut Ui, r: &Report) {
    heading(ui, "UNDERWATER · LAGGED VOLATILITY REGIME RIBBON");
    let xs = &r.underwater;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), dp(9.0)), Sense::hover());
    let painter = ui.painter_at(rect);
    let columns = (rect.width().ceil() as usize).max(1).min(xs.len());
    let width = rect.width() / columns as f32;
    for column in 0..columns {
        let x = &xs[column * xs.len() / columns];
        painter.rect_filled(
            Rect::from_min_size(
                pos2(rect.left() + column as f32 * width, rect.top()),
                vec2(width.max(1.0), rect.height()),
            ),
            0.0,
            x.regime.map(regime_color).unwrap_or(theme::grid()),
        );
    }
    let low = xs.iter().map(|p| p.depth).fold(-0.01, f64::min) * 1.1;
    let p = Plot::new(
        ui,
        195.0,
        [0.0, (xs.len() - 1) as f64],
        [low, 0.0],
        |x| date(xs[(x.round() as usize).min(xs.len() - 1)].ts),
        pct,
    );
    // Fill to the zero line with slim vertical strips: bounded, crisp and cheap on large histories.
    let columns = p.rect.width().ceil().max(1.0) as usize;
    for column in 0..columns {
        let i = column * (xs.len() - 1) / columns;
        let point = p.pos(i as f64, xs[i].depth);
        p.painter
            .with_clip_rect(p.rect)
            .line_segment([pos2(point.x, p.rect.top()), point], Stroke::new(1.0, theme::down().gamma_multiply(0.18)));
    }
    p.line(xs.iter().enumerate().map(|(i, x)| (i as f64, x.depth)), theme::down());
    if let Some(x) = p.hover_x() {
        let v = &xs[(x.round() as usize).min(xs.len() - 1)];
        p.response.on_hover_ui(|ui| {
            ui.label(format!(
                "{}\nDrawdown {}\nLagged regime {}",
                stamp(v.ts),
                pct(v.depth),
                v.regime.map(|k| REGIMES[k]).unwrap_or("warming up")
            ));
        });
    }
    ui.horizontal_wrapped(|ui| {
        for (i, name) in REGIMES.iter().enumerate() {
            ui.colored_label(regime_color(i), format!("■ {name}"));
        }
        ui.colored_label(muted(), "■ Warm-up");
    });
}
fn volatility(ui: &mut Ui, r: &Report) {
    heading(ui, "20-BAR REALIZED VOLATILITY · ANNUALISED");
    let xs = &r.underwater;
    let Some(high) = xs.iter().filter_map(|p| p.rolling_vol).reduce(f64::max) else {
        ui.label(RichText::new("Needs 21 closed bars for rolling vol; 22 for a lagged regime return.").color(muted()));
        return;
    };
    let p = Plot::new(
        ui,
        155.0,
        [0.0, (xs.len() - 1) as f64],
        [0.0, (high * 1.15).max(0.001)],
        |x| date(xs[(x.round() as usize).min(xs.len() - 1)].ts),
        pct,
    );
    if let Some([a, b]) = r.vol_terciles {
        for level in [a, b] {
            p.line([(0.0, level), ((xs.len() - 1) as f64, level)].into_iter(), theme::amber().gamma_multiply(0.5));
        }
    }
    p.line(xs.iter().enumerate().filter_map(|(i, x)| x.rolling_vol.map(|v| (i as f64, v))), theme::cyan());
    if let Some(x) = p.hover_x() {
        let v = &xs[(x.round() as usize).min(xs.len() - 1)];
        p.response.on_hover_ui(|ui| {
            ui.label(format!(
                "{}\n20-bar ann. vol {}",
                stamp(v.ts),
                v.rolling_vol.map(pct).unwrap_or_else(|| "warming up".into())
            ));
        });
    }
}
fn drawdown_table(ui: &mut Ui, r: &Report) {
    heading(ui, "TOP 5 DRAWDOWNS · PEAK → RECOVERY OR LATEST BAR");
    if r.drawdowns.is_empty() {
        ui.label(RichText::new("No below-peak episodes in this window.").color(muted()));
        return;
    }
    egui::ScrollArea::horizontal().id_salt("dist-dd-table").show(ui, |ui| {
        egui::Grid::new("dist-dd").striped(true).spacing(vec2(dp(16.0), dp(5.0))).show(ui, |ui| {
            for s in ["Peak", "Trough", "Recovery", "Depth", "Bars", "Calendar days"] {
                ui.label(RichText::new(s).color(muted()));
            }
            ui.end_row();
            for d in &r.drawdowns {
                ui.label(stamp(d.peak));
                ui.label(stamp(d.trough));
                ui.label(d.recovery.map(stamp).unwrap_or_else(|| "Unrecovered".into()));
                ui.colored_label(theme::down(), pct(d.depth));
                ui.label(d.duration_bars.to_string());
                ui.label(format!("{:.1}", d.duration_days));
                ui.end_row();
            }
        });
    });
}
fn regime_table(ui: &mut Ui, r: &Report) {
    heading(ui, "VOLATILITY REGIMES · RETURN STATISTICS");
    ui.label(RichText::new("Full-window vol terciles are descriptive; labels use the preceding 20-return window. Tied thresholds are Normal. Thresholds use the whole sample, so these are not trading signals.").size(11.0).color(muted()));
    if let Some([a, b]) = r.vol_terciles {
        ui.label(
            RichText::new(format!("Tercile thresholds: {} / {} annualised", pct(a), pct(b)))
                .font(mono(11.0))
                .color(muted()),
        );
    }
    egui::ScrollArea::horizontal().id_salt("dist-regime-table").show(ui, |ui| {
        egui::Grid::new("dist-regimes").striped(true).spacing(vec2(dp(18.0), dp(5.0))).show(ui, |ui| {
            for s in ["Regime", "n", "Mean / bar", "Ann. vol", "Hit rate"] {
                ui.label(RichText::new(s).color(muted()));
            }
            ui.end_row();
            for (i, s) in r.regimes.iter().enumerate() {
                ui.colored_label(regime_color(i), REGIMES[i]);
                ui.label(s.n.to_string());
                for v in [s.mean, s.annual_vol, s.hit_rate] {
                    ui.label(v.map(pct).unwrap_or_else(|| "—".into()));
                }
                ui.end_row();
            }
        });
    });
    heading(ui, "REGIME TRANSITIONS · ROW = FROM, COLUMN = TO");
    egui::ScrollArea::horizontal().id_salt("dist-transition-table").show(ui, |ui| {
        egui::Grid::new("dist-transitions").spacing(vec2(dp(10.0), dp(5.0))).show(ui, |ui| {
            ui.label("");
            for (i, s) in REGIMES.iter().enumerate() {
                ui.colored_label(regime_color(i), *s);
            }
            ui.end_row();
            for (i, row) in r.transitions.iter().enumerate() {
                ui.colored_label(regime_color(i), REGIMES[i]);
                let n: usize = row.iter().sum();
                for (j, count) in row.iter().enumerate() {
                    let probability = if n > 0 { *count as f64 / n as f64 } else { 0.0 };
                    egui::Frame::new()
                        .fill(regime_color(j).gamma_multiply((0.08 + probability * 0.3) as f32))
                        .inner_margin(dp(6.0))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(if n == 0 {
                                    "— · n=0".into()
                                } else {
                                    format!("{:.1}% · n={count}", probability * 100.0)
                                })
                                .font(mono(11.0))
                                .color(text()),
                            );
                        });
                }
                ui.end_row();
            }
        });
    });
}
fn extremes(ui: &mut Ui, r: &Report, k: usize) {
    egui::ScrollArea::horizontal().id_salt("dist-extremes-table").show(ui, |ui| {
        egui::Grid::new("dist-extremes").striped(true).spacing(vec2(dp(15.0), dp(4.0))).show(ui, |ui| {
            for s in ["Best timestamp", "Log return", "Worst timestamp", "Log return"] {
                ui.label(RichText::new(s).color(muted()));
            }
            ui.end_row();
            for (best, worst) in r.best.iter().zip(&r.worst).take(k) {
                ui.label(stamp(best.ts));
                ui.colored_label(theme::up(), pct(best.value));
                ui.label(stamp(worst.ts));
                ui.colored_label(theme::down(), pct(worst.value));
                ui.end_row();
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layouts::PaneSettings;
    fn fixture() -> Vec<Bar> {
        (0..30)
            .map(|i| {
                Bar::new(
                    1_600_000_000 + i * 86_400,
                    10.0 + i as f64,
                    10.0 + i as f64,
                    10.0 + i as f64,
                    10.0 + i as f64,
                    1.0,
                )
                .with_provenance(tr_core::Provenance::Real)
            })
            .collect()
    }
    #[test]
    fn pane_settings_round_trip() {
        let settings = PaneSettings::StudioDist {
            symbol: "AAPL US Equity".into(),
            timeframe: Timeframe::H1,
            window: Window::ThreeYears,
        };
        let json = serde_json::to_string(&settings).unwrap();
        let loaded: PaneSettings = serde_json::from_str(&json).unwrap();
        let pane = PaneKind::from_settings(loaded);
        assert_eq!(pane.code(), "DIST");
        assert_eq!(serde_json::to_value(pane.settings()).unwrap(), serde_json::to_value(settings).unwrap());
        let PaneKind::StudioDist(pane) = pane else { panic!("DIST settings") };
        assert_eq!(pane.timeframe, Timeframe::H1);
        assert_eq!(pane.window, Window::ThreeYears);
        assert!(pane.report.is_none() && !pane.loading);
    }
    #[test]
    fn pane_generation_and_provenance_gates() {
        let mut pane = DistPane::new("AAPL", Timeframe::D1, Window::All);
        pane.generation = 2;
        pane.loading = true;
        let mut report = dist::compute("AAPL", &fixture(), Timeframe::D1, Window::All, "test", None).unwrap();
        pane.accept(1, Ok(report.clone()));
        assert!(pane.loading && pane.report.is_none());
        pane.accept(2, Ok(report.clone()));
        assert!(!pane.loading && pane.report.is_some());
        for provenance in [tr_core::Provenance::Sample, tr_core::Provenance::Missing] {
            report.provenance = provenance;
            pane.accept(2, Ok(report.clone()));
            assert!(pane.report.is_none() && pane.error.is_some());
        }
        pane.accept(1, Err("old error".into()));
        assert_ne!(pane.error.as_deref(), Some("old error"));
    }
    #[test]
    fn command_opens_distribution_pane() {
        let mut app = super::super::tests::test_app();
        app.apply(crate::command::Function::StudioDist { timeframe: Timeframe::H1 });
        assert!(app.panes.iter().any(|p| matches!(&p.kind,PaneKind::StudioDist(d) if d.timeframe==Timeframe::H1)));
    }
    #[tokio::test]
    async fn pending_intraday_bar_is_excluded_without_fresh_clock() {
        let bars = fixture();
        let r = build_report("AAPL".into(), bars, Timeframe::H1, Window::All, "vault only".into(), None).await.unwrap();
        assert_eq!(r.bars, 29);
        assert_eq!(r.observed_at, None);
    }
    #[tokio::test]
    async fn pending_sample_bar_is_refused_before_trimming() {
        let mut bars = fixture();
        bars.last_mut().unwrap().provenance = tr_core::Provenance::Sample;
        assert!(build_report("AAPL".into(), bars, Timeframe::H1, Window::All, "unit".into(), None).await.is_err());
    }
    #[test]
    fn sample_mode_clears_existing_results_and_invalidates_pending_work() {
        let mut pane = DistPane::new("AAPL", Timeframe::D1, Window::All);
        pane.generation = 2;
        let report = dist::compute("AAPL", &fixture(), Timeframe::D1, Window::All, "unit", None).unwrap();
        pane.report = Some(report.clone());
        pane.loading = true;
        pane.deny_sample();
        pane.accept(2, Ok(report));
        assert!(pane.report.is_none() && !pane.loading);
        assert!(pane.error.as_deref().unwrap().contains("no real data"));
        assert_eq!(pane.generation, 3);
    }
    #[test]
    fn new_pane_timeframe_is_not_interpreted_as_a_ticker() {
        let app = super::super::tests::test_app();
        let PaneKind::StudioDist(pane) = app.panel_from_spec("DIST 1H", Some("AAPL")).unwrap() else {
            panic!("DIST pane")
        };
        assert_eq!(pane.symbol, "AAPL US Equity");
        assert_eq!(pane.timeframe, Timeframe::H1);
        let PaneKind::StudioDist(pane) = app.panel_from_spec("RDIST 1m", Some("AAPL")).unwrap() else {
            panic!("DIST pane")
        };
        assert_eq!(pane.timeframe, Timeframe::M1);
    }
    #[test]
    fn charts_paint_finite_geometry_in_narrow_and_wide_panes() {
        let report = dist::compute("AAPL", &fixture(), Timeframe::D1, Window::All, "unit", None).unwrap();
        for width in [320.0, 1440.0] {
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1440.0))),
                    ..Default::default()
                },
                |ui| {
                    histogram(ui, &report);
                    qq(ui, &report);
                    underwater(ui, &report);
                    volatility(ui, &report);
                },
            );
            output.textures_delta.clear();
            let meshes = ctx.tessellate(output.shapes, output.pixels_per_point);
            assert!(!meshes.is_empty());
            for primitive in meshes {
                if let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive {
                    assert!(mesh.vertices.iter().all(|v| v.pos.x.is_finite() && v.pos.y.is_finite()));
                }
            }
        }
    }
}
