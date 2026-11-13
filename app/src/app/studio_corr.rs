//! Relationship Studio pane and its archive/vault loading orchestration.
use super::{PaneKind, TerminalApp};
use crate::{congress_view::Action, dock::PaneId, theme, worker::Request};
use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, Ui};
use tr_core::{Bar, Timeframe};
use tr_data::studio_corr::{self as maths, CorrReport, NamedBars};

pub struct CorrPane {
    pub symbol: String,
    pub spec: String,
    pub generation: u64,
    pub loading: bool,
    pub error: Option<String>,
    pub report: Option<CorrReport>,
    /// Next request bypasses the bar cache (set by the Reload button).
    fresh: bool,
}

impl CorrPane {
    pub fn new(symbol: impl Into<String>, spec: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            spec: spec.into(),
            generation: 0,
            loading: false,
            error: None,
            report: None,
            fresh: false,
        }
    }
    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        self.generation += 1;
        self.report = None;
        self.loading = false;
        self.error = None;
        if self.symbol.trim().is_empty() {
            self.error = Some("CORR needs a ticker: AAPL CORR [QQQ] [1H]. Sample prices are never used.".into());
            return;
        }
        if let Err(e) = parse_spec(&self.spec) {
            self.error = Some(e);
            return;
        }
        self.loading = true;
        worker.send(Request::StudioCorr {
            pane,
            generation: self.generation,
            symbol: tr_data::security(&self.symbol).key(),
            spec: self.spec.clone(),
            fresh: std::mem::take(&mut self.fresh),
        });
    }
    pub fn accept(&mut self, generation: u64, result: Result<CorrReport, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        self.report = None;
        match result {
            Ok(r) => match r.provenance.require_verified("Relationship Studio") {
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

pub(crate) fn parse_spec(spec: &str) -> Result<(String, Timeframe), String> {
    let mut tokens: Vec<_> = spec.split_whitespace().collect();
    let tf = tokens.last().and_then(|t| t.parse::<Timeframe>().ok()).unwrap_or(Timeframe::D1);
    if tokens.last().is_some_and(|t| t.parse::<Timeframe>().is_ok()) {
        tokens.pop();
    }
    if tf != Timeframe::D1 && !tf.is_intraday() {
        return Err("CORR supports daily or intraday bars, e.g. CORR QQQ 1H.".into());
    }
    if tokens.len() > 1
        || tokens
            .first()
            .is_some_and(|t| !t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '^' | '=')))
    {
        return Err("Use CORR [benchmark ticker] [timeframe], e.g. AAPL CORR QQQ 1H. Default benchmark: SPY.".into());
    }
    Ok((tr_data::security(tokens.first().copied().unwrap_or("SPY")).key(), tf))
}

/// Concurrent symbol loads per CORR request (asset, benchmark and peers).
pub(crate) const CORR_CONCURRENCY: usize = 4;
const CACHE_CAPACITY: usize = 64;

/// Verified, closed-bar series for one (symbol, interval), as loaded.
#[derive(Clone)]
struct Loaded {
    bars: Vec<Bar>,
    source: String,
    gaps: Vec<(i64, i64)>,
}

/// Successful real-data loads keyed by (symbol, interval). Failures are never
/// cached, and nothing synthetic is ever stored. Re-opening CORR inside the TTL
/// reuses the bars; Reload bypasses the cache.
#[derive(Default)]
pub(crate) struct CorrCache {
    entries: std::sync::Mutex<std::collections::HashMap<(String, Timeframe), (std::time::Instant, Loaded)>>,
}
pub(crate) static CORR_CACHE: std::sync::LazyLock<CorrCache> = std::sync::LazyLock::new(CorrCache::default);

/// Daily archives change once a session; intraday entries live about one bar
/// (1–10 minutes), so a cached series is at most one closed bar behind.
pub(crate) fn cache_ttl(tf: Timeframe) -> std::time::Duration {
    std::time::Duration::from_secs(if tf == Timeframe::D1 { 900 } else { tf.seconds().clamp(60, 600) as u64 })
}

impl CorrCache {
    fn get(&self, key: &str, tf: Timeframe) -> Option<Loaded> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(&(key.to_string(), tf))
            .filter(|(at, _)| at.elapsed() < cache_ttl(tf))
            .map(|(_, loaded)| loaded.clone())
    }
    fn put(&self, key: String, tf: Timeframe, loaded: Loaded) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|(_, tf), (at, _)| at.elapsed() < cache_ttl(*tf));
        if entries.len() >= CACHE_CAPACITY
            && let Some(oldest) = entries.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone())
        {
            entries.remove(&oldest);
        }
        entries.insert((key, tf), (std::time::Instant::now(), loaded));
    }
}

/// Kept here so the shared worker only registers a request/response arm. The supplied
/// loader is the existing vault-aware analysis_bars path (history_sourced underneath).
/// Symbols load concurrently (bounded by [`CORR_CONCURRENCY`]) through `cache`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn research<F, Fut>(
    cache: &CorrCache,
    fresh: bool,
    history: &tr_data::history::Service,
    bridge: &tr_data::whats_news::Bridge,
    sample: bool,
    symbol: &str,
    spec: &str,
    load_intraday: F,
) -> anyhow::Result<CorrReport>
where
    F: Fn(String, Timeframe) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<(Vec<Bar>, tr_data::vault::StitchInfo)>>,
{
    use futures::StreamExt;
    if sample {
        anyhow::bail!("Relationship Studio: no real data in sample / demo display mode. Sample prices are never used.");
    }
    let (benchmark, tf) = parse_spec(spec).map_err(anyhow::Error::msg)?;
    let symbol = tr_data::security(symbol).key();
    let mut names = vec![symbol.clone(), benchmark.clone()];
    for ticker in maths::DEFAULT_SET {
        let key = tr_data::security(ticker).key();
        if !names.contains(&key) {
            names.push(key);
        }
    }
    names.dedup();
    let load = |key: String| {
        let load_intraday = &load_intraday;
        async move {
            if !fresh && let Some(hit) = cache.get(&key, tf) {
                return (key, Ok(hit));
            }
            let result = if tf == Timeframe::D1 {
                daily(history, bridge, &key).await.map(|(bars, source)| Loaded { bars, source, gaps: Vec::new() })
            } else {
                load_intraday(key.clone(), tf).await.and_then(|(mut bars, info)| {
                    tr_data::stats::require_research_bars(&bars, "Relationship intraday")
                        .map_err(anyhow::Error::msg)?;
                    let n = tr_data::closed::closed_len(tf, &bars, tr_data::security(&key).class, info.fetched_at);
                    bars.truncate(n);
                    Ok(Loaded { bars, source: info.label(), gaps: info.gaps })
                })
            };
            if let Ok(loaded) = &result {
                cache.put(key.clone(), tf, loaded.clone());
            }
            (key, result)
        }
    };
    let loaded: Vec<(String, anyhow::Result<Loaded>)> =
        futures::stream::iter(names.into_iter().map(load)).buffered(CORR_CONCURRENCY).collect().await;
    let rows: Vec<NamedBars> = loaded
        .into_iter()
        .map(|(key, result)| match result {
            Ok(Loaded { bars, source, gaps }) => NamedBars { symbol: key, bars, source, error: None, gaps },
            // A failed ticker is listed as unavailable; never substituted.
            Err(e) => NamedBars { symbol: key, error: Some(format!("{e:#}")), ..Default::default() },
        })
        .collect();
    let asset = rows.iter().find(|r| r.symbol == symbol).unwrap();
    let other = rows.iter().find(|r| r.symbol == benchmark).unwrap();
    for row in [asset, other] {
        if let Some(e) = &row.error {
            anyhow::bail!("{} unavailable: {}", tr_data::security(&row.symbol).ticker, e);
        }
    }
    let source = rows
        .iter()
        .filter(|r| r.error.is_none())
        .map(|r| format!("{}: {}", tr_data::security(&r.symbol).ticker, r.source))
        .collect::<Vec<_>>()
        .join(" · ");
    // Same per-timeframe / asset-class convention as Distribution Studio.
    let options = maths::Options {
        annual_periods: tr_data::studio_dist::periods_per_year(tf, tr_data::security(&symbol).class),
        daily: tf == Timeframe::D1,
    };
    let (a, b) = (asset.clone(), other.clone());
    tokio::task::spawn_blocking(move || maths::compute_series(&a, &b, &rows, options, &source))
        .await?
        .map_err(anyhow::Error::msg)
}

async fn daily(
    history: &tr_data::history::Service,
    bridge: &tr_data::whats_news::Bridge,
    key: &str,
) -> anyhow::Result<(Vec<Bar>, String)> {
    let ticker = tr_data::whats_news::archive_ticker(key);
    let wanted = ticker.clone();
    let mut bars = history.db(move |db| db.daily(&wanted)).await?;
    let mut source = "Local Yahoo daily research archive".to_string();
    if bars.is_empty() && bridge.enabled {
        let external = bridge.histories_for(std::slice::from_ref(&ticker)).await?;
        external.provenance.require_verified("Relationship archive").map_err(anyhow::Error::msg)?;
        if let Some(skip) = external.data.skipped.iter().find(|s| s.symbol.eq_ignore_ascii_case(&ticker)) {
            anyhow::bail!("{}: {}", skip.symbol, skip.reason);
        }
        source = external.source;
        bars = external
            .data
            .bars
            .into_iter()
            .find(|(s, _)| s.eq_ignore_ascii_case(&ticker))
            .map(|(_, b)| b)
            .unwrap_or_default();
    }
    tr_data::stats::require_research_bars(&bars, "Relationship daily archive").map_err(anyhow::Error::msg)?;
    let n = tr_data::closed::closed_len(
        Timeframe::D1,
        &bars,
        tr_data::security(key).class,
        Some(chrono::Utc::now().timestamp()),
    );
    bars.truncate(n);
    Ok((bars, source))
}

impl TerminalApp {
    pub(super) fn open_studio_corr(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        self.status = "Relationship Studio · common closed timestamps · verified data".into();
        self.open_panel(PaneKind::StudioCorr(Box::new(CorrPane::new(symbol, spec))));
    }
    pub(super) fn studio_corr_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("relationship pane");
        let PaneKind::StudioCorr(pane) = &mut self.panes[i].kind else { return vec![] };
        let mut reload = false;
        ui.horizontal_wrapped(|ui| {
            super::studio_dist::studio_title(ui, "RELATIONSHIP STUDIO");
            if ui.small_button("Reload").on_hover_text("Re-read every series, bypassing the short bar cache").clicked() {
                reload = true;
                pane.fresh = true;
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Asset");
            let a = ui.add(egui::TextEdit::singleline(&mut pane.symbol).desired_width(100.0).hint_text("AAPL"));
            ui.label("Benchmark / interval");
            let b = ui.add(egui::TextEdit::singleline(&mut pane.spec).desired_width(110.0).hint_text("SPY 1D"));
            reload |= (a.lost_focus() || b.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
            reload |= ui.small_button("Apply").clicked();
        });
        ui.label(
            RichText::new("Log returns · common closed timestamps · no fill-forward · default SPY")
                .font(theme::ui_font(11.0))
                .color(theme::muted()),
        );
        if self.provider.eq_ignore_ascii_case("sample") {
            pane.report = None;
            pane.loading = false;
            pane.error = Some(
                "No real data in sample / demo display mode. Relationship Studio never uses sample prices.".into(),
            );
        } else if reload || (!pane.loading && pane.report.is_none() && pane.error.is_none()) {
            pane.request(id, &self.worker);
        }
        super::studio_dist::studio_states(ui, pane.loading, pane.error.as_deref(), None);
        if let Some(r) = &pane.report {
            ui.push_id(id, |ui| report_view(ui, id, r));
        }
        vec![]
    }
}

fn stamp(ts: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn number(v: Option<f64>) -> String {
    v.map(|v| format!("{v:.3}")).unwrap_or_else(|| "—".into())
}
fn percent(v: Option<f64>) -> String {
    v.map(|v| format!("{:.2}%", v * 100.0)).unwrap_or_else(|| "—".into())
}
fn heading(ui: &mut Ui, title: &str) {
    super::studio_dist::studio_heading(ui, title);
}

/// Gap banner pinned above the scrolling report.
fn report_view(ui: &mut Ui, id: PaneId, r: &CorrReport) {
    super::studio_dist::gap_warning(ui, &r.gaps);
    egui::ScrollArea::vertical().id_salt(("studio-corr", id)).auto_shrink([false, false]).show(ui, |ui| render(ui, r));
}

fn render(ui: &mut Ui, r: &CorrReport) {
    let m = &r.ols;
    ui.label(
        RichText::new(format!(
            "{} / {}  ·  {}  ·  {} … {}  ·  N={} common returns",
            tr_data::security(&r.symbol).ticker,
            tr_data::security(&r.benchmark).ticker,
            r.provenance.label(),
            tr_data::stats::bar_date(r.path[0].ts),
            tr_data::stats::bar_date(r.as_of),
            m.n
        ))
        .font(theme::mono(11.0))
        .color(theme::muted()),
    )
    .on_hover_text(&r.source);
    ui.label(
        RichText::new(format!(
            "{} common / {} asset / {} benchmark price bars",
            r.aligned_bars, r.asset_bars, r.benchmark_bars
        ))
        .color(theme::muted()),
    );
    ui.horizontal_wrapped(|ui| {
        for (label, value, color) in [
            ("BETA", number(m.beta), theme::cyan()),
            ("CORRELATION", number(m.correlation), theme::up()),
            ("R²", number(m.r_squared), theme::amber()),
            ("ALPHA / YR", percent(m.alpha_annual), theme::text()),
        ] {
            ui.group(|ui| {
                ui.label(RichText::new(label).font(theme::mono(10.0)).color(theme::muted()));
                ui.label(RichText::new(value).font(theme::mono(22.0)).strong().color(color));
            });
        }
    });
    ui.label(
        RichText::new(format!(
            "α SE {}  t {}   ·   β SE {}  t {}   ·   Residual vol / yr {}",
            number(m.alpha_se),
            number(m.alpha_t),
            number(m.beta_se),
            number(m.beta_t),
            percent(m.residual_vol_annual)
        ))
        .font(theme::mono(11.5)),
    );
    ui.label(
        RichText::new(format!(
            "Up capture {} · β {} · n={}    Down capture {} · β {} · n={}",
            percent(m.up_capture),
            number(m.upside_beta),
            m.up_n,
            percent(m.down_capture),
            number(m.downside_beta),
            m.down_n
        ))
        .font(theme::mono(11.5)),
    );
    ui.label(
        RichText::new(format!(
            "OLS of asset on benchmark log returns over common closed timestamps (dates for daily bars); \
             non-overlapping one-bar returns, classical (iid, n−2) standard errors, no HAC, no risk-free rate. \
             α / yr = per-bar α × {:.0} periods/year. Capture = mean asset ÷ mean benchmark log return on \
             benchmark up / down bars. Undefined values = —.",
            r.annual_periods
        ))
        .font(theme::ui_font(10.5))
        .color(theme::muted()),
    );
    heading(ui, "RETURN CLOUD · CYAN = MOST RECENT 20");
    scatter(ui, r);
    for correlation in [false, true] {
        heading(
            ui,
            if correlation {
                "ROLLING CORRELATION · 20 / 60 / 120 RETURNS"
            } else {
                "ROLLING BETA · 20 / 60 / 120 RETURNS"
            },
        );
        let lines: Vec<_> = r
            .rolling
            .iter()
            .enumerate()
            .map(|(i, roll)| PlotLine {
                label: format!("{} returns", roll.window),
                color: theme::series(i),
                points: roll
                    .points
                    .iter()
                    .map(|&(ts, beta, corr)| (ts as f64, if correlation { corr } else { beta }))
                    .collect(),
            })
            .collect();
        line_chart(ui, &lines, false, true);
    }
    heading(ui, "LOG PRICE RATIO · LEVEL");
    line_chart(
        ui,
        &[PlotLine {
            label: "ln(asset / benchmark)".into(),
            color: theme::cyan(),
            points: r.path.iter().map(|p| (p.ts as f64, Some(p.log_ratio))).collect(),
        }],
        false,
        true,
    );
    heading(ui, "RATIO Z-SCORE · VS PRIOR 60 COMMON PRICES (CURRENT EXCLUDED)");
    line_chart(
        ui,
        &[PlotLine {
            label: "z60".into(),
            color: theme::amber(),
            points: r.path.iter().map(|p| (p.ts as f64, p.z60)).collect(),
        }],
        false,
        true,
    );
    heading(ui, "IDIOSYNCRATIC CUMULATIVE RETURN");
    line_chart(
        ui,
        &[PlotLine {
            label: "exp(Σ OLS residual) − 1".into(),
            color: theme::up(),
            points: r.path.iter().map(|p| (p.ts as f64, p.residual_return)).collect(),
        }],
        true,
        true,
    );
    ui.label(RichText::new("In-sample residuals use the full-sample fit and sum to approximately zero. This path is descriptive and uses future fit information; the ratio z-score is not a cointegration test. Missing timestamps can make a return span several bars; no return spans a vault gap.").font(theme::ui_font(10.5)).color(theme::muted()));
    heading(ui, "CORRELATION MATRIX · PAIRWISE COMMON RETURNS");
    matrix(ui, &r.matrix);
}

struct PlotLine {
    label: String,
    color: Color32,
    points: Vec<(f64, Option<f64>)>,
}
struct Plot {
    rect: Rect,
    x: (f64, f64),
    y: (f64, f64),
}
impl Plot {
    fn pos(&self, x: f64, y: f64) -> Pos2 {
        egui::pos2(
            self.rect.left() + ((x - self.x.0) / (self.x.1 - self.x.0)) as f32 * self.rect.width(),
            self.rect.bottom() - ((y - self.y.0) / (self.y.1 - self.y.0)) as f32 * self.rect.height(),
        )
    }
}
fn bounds(values: impl Iterator<Item = f64>, include_zero: bool) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for v in values.filter(|v| v.is_finite()) {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if !lo.is_finite() {
        return (-1.0, 1.0);
    }
    if include_zero {
        lo = lo.min(0.0);
        hi = hi.max(0.0);
    }
    let pad = ((hi - lo) * 0.08).max(1e-8);
    (lo - pad, hi + pad)
}
fn axes(ui: &mut Ui, x: (f64, f64), y: (f64, f64), pct: bool, dates: bool) -> (Plot, egui::Response) {
    let (outer, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width().max(80.0), theme::dp(180.0)), Sense::hover());
    let rect = Rect::from_min_max(outer.min + egui::vec2(48.0, 8.0), outer.max - egui::vec2(10.0, 24.0));
    let p = Plot { rect, x, y };
    let painter = ui.painter_at(outer);
    painter.rect_filled(outer, 3.0, theme::card());
    for i in 0..=4 {
        let y = p.y.0 + (p.y.1 - p.y.0) * i as f64 / 4.0;
        let py = p.pos(x.0, y).y;
        painter
            .line_segment([egui::pos2(rect.left(), py), egui::pos2(rect.right(), py)], Stroke::new(0.5, theme::grid()));
        painter.text(
            egui::pos2(rect.left() - 4.0, py),
            egui::Align2::RIGHT_CENTER,
            if pct { format!("{:.1}%", y * 100.0) } else { format!("{y:.2}") },
            theme::mono(9.0),
            theme::muted(),
        );
        let x = p.x.0 + (p.x.1 - p.x.0) * i as f64 / 4.0;
        let px = p.pos(x, p.y.0).x;
        painter
            .line_segment([egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())], Stroke::new(0.5, theme::grid()));
        // Keep date labels legible in a narrow dock.
        if i == 0 || i == 4 || (rect.width() > 450.0 && i == 2) {
            let label = if dates { tr_data::stats::bar_date(x as i64) } else { format!("{:.1}%", x * 100.0) };
            painter.text(
                egui::pos2(px, rect.bottom() + 4.0),
                if i == 0 {
                    egui::Align2::LEFT_TOP
                } else if i == 4 {
                    egui::Align2::RIGHT_TOP
                } else {
                    egui::Align2::CENTER_TOP
                },
                label,
                theme::mono(9.0),
                theme::muted(),
            );
        }
    }
    if y.0 <= 0.0 && y.1 >= 0.0 {
        painter.line_segment([p.pos(x.0, 0.0), p.pos(x.1, 0.0)], Stroke::new(1.0, theme::border()));
    }
    (p, response)
}
fn line_chart(ui: &mut Ui, lines: &[PlotLine], pct: bool, dates: bool) {
    if !lines.iter().any(|l| l.points.iter().any(|p| p.1.is_some())) {
        ui.label(RichText::new("Not enough common observations, or zero variance.").color(theme::muted()));
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for line in lines {
            ui.label(RichText::new(&line.label).font(theme::mono(10.0)).color(line.color));
        }
    });
    let x = bounds(lines.iter().flat_map(|l| l.points.iter().filter(|p| p.1.is_some()).map(|p| p.0)), false);
    let y = bounds(lines.iter().flat_map(|l| l.points.iter().filter_map(|p| p.1)), true);
    let (plot, response) = axes(ui, x, y, pct, dates);
    let painter = ui.painter_at(plot.rect);
    for line in lines {
        for w in line.points.windows(2) {
            if let (Some(a), Some(b)) = (w[0].1, w[1].1) {
                painter.line_segment([plot.pos(w[0].0, a), plot.pos(w[1].0, b)], Stroke::new(1.6, line.color));
            }
        }
    }
    if let Some(pos) = response.hover_pos().filter(|p| plot.rect.contains(*p)) {
        let x = plot.x.0 + (pos.x - plot.rect.left()) as f64 / plot.rect.width() as f64 * (plot.x.1 - plot.x.0);
        let mut labels = Vec::new();
        for line in lines {
            if let Some(&(ts, Some(v))) =
                line.points.iter().filter(|p| p.1.is_some()).min_by(|a, b| (a.0 - x).abs().total_cmp(&(b.0 - x).abs()))
            {
                let p = plot.pos(ts, v);
                painter.circle_filled(p, 3.0, line.color);
                labels.push(format!(
                    "{} · {}: {}",
                    stamp(ts as i64),
                    line.label,
                    if pct { percent(Some(v)) } else { number(Some(v)) }
                ));
            }
        }
        response.on_hover_text(labels.join("\n"));
    }
}
fn scatter(ui: &mut Ui, r: &CorrReport) {
    let x = bounds(r.returns.iter().map(|p| p.benchmark), true);
    let mut ys: Vec<_> = r.returns.iter().map(|p| p.asset).collect();
    if let (Some(a), Some(b), Some(s)) = (r.ols.alpha, r.ols.beta, r.ols.residual_sigma) {
        for x in [x.0, x.1] {
            ys.extend([a + b * x - 2.0 * s, a + b * x + 2.0 * s]);
        }
    }
    let (plot, response) = axes(ui, x, bounds(ys.into_iter(), true), true, false);
    let painter = ui.painter_at(plot.rect);
    if let (Some(a), Some(b), Some(s)) = (r.ols.alpha, r.ols.beta, r.ols.residual_sigma) {
        let polygon = vec![
            plot.pos(x.0, a + b * x.0 - 2.0 * s),
            plot.pos(x.1, a + b * x.1 - 2.0 * s),
            plot.pos(x.1, a + b * x.1 + 2.0 * s),
            plot.pos(x.0, a + b * x.0 + 2.0 * s),
        ];
        painter.add(egui::Shape::convex_polygon(polygon, theme::cyan().linear_multiply(0.10), Stroke::NONE));
        for offset in [-2.0 * s, 0.0, 2.0 * s] {
            painter.line_segment(
                [plot.pos(x.0, a + b * x.0 + offset), plot.pos(x.1, a + b * x.1 + offset)],
                Stroke::new(if offset == 0.0 { 1.7 } else { 0.7 }, theme::cyan()),
            );
        }
    }
    for (i, p) in r.returns.iter().enumerate() {
        let recent = i >= r.returns.len().saturating_sub(20);
        painter.circle_filled(
            plot.pos(p.benchmark, p.asset),
            if recent { 3.0 } else { 2.0 },
            if recent { theme::cyan() } else { theme::muted().linear_multiply(0.45) },
        );
    }
    if let Some(pos) = response.hover_pos().filter(|p| plot.rect.contains(*p))
        && let Some(p) = r.returns.iter().min_by(|a, b| {
            plot.pos(a.benchmark, a.asset).distance_sq(pos).total_cmp(&plot.pos(b.benchmark, b.asset).distance_sq(pos))
        })
    {
        painter.circle_stroke(plot.pos(p.benchmark, p.asset), 5.0, Stroke::new(1.5, theme::amber()));
        response.on_hover_text(format!(
            "{} → {}\nBenchmark x: {}\nAsset y: {}",
            stamp(p.start),
            stamp(p.end),
            percent(Some(p.benchmark)),
            percent(Some(p.asset))
        ));
    }
    ui.label(RichText::new("x: benchmark log return · y: asset log return · band: fitted mean ±2 residual σ (not a confidence interval)").font(theme::ui_font(10.5)).color(theme::muted()));
}
fn matrix(ui: &mut Ui, m: &maths::Matrix) {
    egui::ScrollArea::horizontal().id_salt("corr-matrix").show(ui, |ui| {
        egui::Grid::new("corr-matrix-grid").spacing(egui::vec2(3.0, 3.0)).show(ui, |ui| {
            ui.label("");
            for name in &m.symbols {
                ui.label(RichText::new(tr_data::security(name).ticker).font(theme::mono(10.0)).color(theme::muted()));
            }
            ui.end_row();
            for (i, row) in m.cells.iter().enumerate() {
                ui.label(RichText::new(tr_data::security(&m.symbols[i]).ticker).font(theme::mono(10.0)));
                for cell in row {
                    let color = cell
                        .correlation
                        .map(|r| if r >= 0.0 { theme::up() } else { theme::down() })
                        .unwrap_or(theme::muted());
                    let label = format!(
                        "{}\nn={}",
                        cell.correlation.map(|v| format!("{v:.2}")).unwrap_or_else(|| "—".into()),
                        cell.n
                    );
                    egui::Frame::new()
                        .fill(
                            color.linear_multiply(
                                cell.correlation.map(|v| 0.10 + 0.35 * v.abs() as f32).unwrap_or(0.06),
                            ),
                        )
                        .inner_margin(5.0)
                        .show(ui, |ui| ui.label(RichText::new(label).font(theme::mono(10.0)).color(color)))
                        .inner
                        .on_hover_text(
                            cell.reason
                                .as_deref()
                                .unwrap_or("Pairwise common endpoints; sample size may differ by pair"),
                        );
                }
                ui.end_row();
            }
        });
    });
    for (symbol, reason) in &m.unavailable {
        ui.label(
            RichText::new(format!("{} unavailable: {reason}", tr_data::security(symbol).ticker))
                .font(theme::ui_font(11.0))
                .color(theme::muted()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layouts::PaneSettings;
    #[test]
    fn query_defaults_and_errors() {
        assert_eq!(parse_spec("").unwrap(), (tr_data::security("SPY").key(), Timeframe::D1));
        assert_eq!(parse_spec("30m").unwrap().1, Timeframe::M30);
        assert!(parse_spec("QQQ 1W").is_err());
        assert!(parse_spec("QQQ garbage").is_err());
    }
    #[test]
    fn settings_round_trip_and_old_defaults() {
        let s = PaneSettings::StudioCorr { symbol: "AAPL US Equity".into(), spec: "QQQ 1H".into() };
        let serialized = serde_json::to_string(&s).unwrap();
        let restored = PaneKind::from_settings(serde_json::from_str(&serialized).unwrap());
        assert_eq!(serde_json::to_value(restored.settings()).unwrap(), serde_json::to_value(s).unwrap());
        let PaneKind::StudioCorr(p) = restored else { panic!("relationship kind") };
        assert_eq!(p.spec, "QQQ 1H");
        assert_eq!(p.generation, 0);
        assert!(p.report.is_none());
        let old: PaneSettings = serde_json::from_str(r#"{"StudioCorr":{}}"#).unwrap();
        assert_eq!(old, PaneSettings::StudioCorr { symbol: String::new(), spec: String::new() });
    }
    #[test]
    fn stale_and_unverified_responses_are_refused() {
        let mut pane = CorrPane::new("AAPL", "");
        pane.generation = 2;
        pane.loading = true;
        pane.accept(1, Err("stale".into()));
        assert!(pane.loading);
        assert!(pane.error.is_none());
        let bars: Vec<_> = (0..5)
            .map(|i| {
                let p = 100.0 + i as f64;
                Bar::new(i * 86400, p, p, p, p, 1.0).with_provenance(tr_core::Provenance::Real)
            })
            .collect();
        let mut r = maths::compute("A", "B", &bars, &bars, 252.0, &[], "unit").unwrap();
        r.provenance = tr_core::Provenance::Sample;
        pane.accept(2, Ok(r));
        assert!(pane.report.is_none());
        assert!(pane.error.unwrap().contains("SAMPLE"));
    }

    struct NoNetwork;
    #[async_trait::async_trait]
    impl tr_data::history::DailyHistory for NoNetwork {
        async fn daily(
            &self,
            _: &str,
            _: Option<chrono::NaiveDate>,
            _: chrono::NaiveDate,
        ) -> Result<Vec<Bar>, tr_data::history::Failure> {
            panic!("Relationship tests must never download daily bars");
        }
    }
    fn history(archive: tr_data::history::Archive) -> tr_data::history::Service {
        tr_data::history::Service::new(archive, std::sync::Arc::new(NoNetwork), Default::default())
    }
    fn test_bars(start: i64, step: i64) -> Vec<Bar> {
        (0..6)
            .map(|i| {
                let p = 100.0 + (i * i) as f64;
                Bar::new(start + i * step, p, p, p, p, 1.0).with_provenance(tr_core::Provenance::Real)
            })
            .collect()
    }
    #[tokio::test]
    async fn daily_archive_only_and_sample_empty_state() {
        let start = 1_735_862_400; // 2025-01-03 00:00 UTC
        let mut archive = tr_data::history::Archive::memory().unwrap();
        for ticker in ["AAPL", "SPY"] {
            archive.import_verified(ticker, ticker, &test_bars(start, 86400)).unwrap();
        }
        let history = history(archive);
        let bridge = tr_data::whats_news::Config { enabled: false, ..Default::default() }.bridge();
        let cache = CorrCache::default();
        let r = research(&cache, false, &history, &bridge, false, "AAPL", "", |_, _| async {
            panic!("daily must use archive")
        })
        .await
        .unwrap();
        assert_eq!(r.ols.n, 5);
        assert!(r.source.contains("Local Yahoo daily research archive"));
        assert_eq!(r.matrix.unavailable.len(), 6);
        assert!(r.provenance.is_verified());
        let err = research(&cache, false, &history, &bridge, true, "AAPL", "", |_, _| async {
            panic!("sample must not fetch")
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no real data"));
        let empty = self::history(tr_data::history::Archive::memory().unwrap());
        let err = research(&CorrCache::default(), false, &empty, &bridge, false, "AAPL", "", |_, _| async {
            panic!("daily must not fetch intraday")
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no history"));
    }
    #[tokio::test]
    async fn intraday_uses_observation_clock_and_lists_missing_peer() {
        let history = history(tr_data::history::Archive::memory().unwrap());
        let bridge = tr_data::whats_news::Config { enabled: false, ..Default::default() }.bridge();
        let start = "2025-01-06T14:30:00Z".parse::<chrono::DateTime<chrono::Utc>>().unwrap().timestamp();
        let r = research(&CorrCache::default(), false, &history, &bridge, false, "AAPL", "SPY 30m", |key, tf| async move {
            assert_eq!(tf, Timeframe::M30);
            if tr_data::security(&key).ticker == "UUP" {
                anyhow::bail!("missing verified fixture");
            }
            Ok((
                test_bars(start, 1800),
                tr_data::vault::StitchInfo {
                    fetched_at: Some(start + 5 * 1800 + 900),
                    source: "verified fixture".into(),
                    ..Default::default()
                },
            ))
        })
        .await
        .unwrap();
        assert_eq!(r.aligned_bars, 5); // Sixth bar is still forming at the observation clock.
        assert_eq!(r.ols.n, 4);
        assert_eq!(r.as_of, start + 4 * 1800);
        assert_eq!(r.matrix.unavailable.len(), 1);
        assert!(r.matrix.unavailable[0].0.starts_with("UUP"));
    }

    fn intraday_fixture(start: i64, gaps: Vec<(i64, i64)>) -> (Vec<Bar>, tr_data::vault::StitchInfo) {
        (
            test_bars(start, 1800),
            tr_data::vault::StitchInfo {
                fetched_at: Some(start + 6 * 1800),
                source: "verified fixture".into(),
                gaps,
                ..Default::default()
            },
        )
    }

    #[tokio::test]
    async fn loads_run_concurrently_bounded_and_are_cached() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let history = history(tr_data::history::Archive::memory().unwrap());
        let bridge = tr_data::whats_news::Config { enabled: false, ..Default::default() }.bridge();
        let start = "2025-01-06T14:30:00Z".parse::<chrono::DateTime<chrono::Utc>>().unwrap().timestamp();
        let (calls, live, peak, uup) =
            (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
        let cache = CorrCache::default();
        let run = |fresh: bool| {
            research(&cache, fresh, &history, &bridge, false, "AAPL", "SPY 30m", |key, _| {
                let (calls, live, peak, uup) = (&calls, &live, &peak, &uup);
                async move {
                    calls.fetch_add(1, SeqCst);
                    let now = live.fetch_add(1, SeqCst) + 1;
                    peak.fetch_max(now, SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    live.fetch_sub(1, SeqCst);
                    if tr_data::security(&key).ticker == "UUP" {
                        uup.fetch_add(1, SeqCst);
                        anyhow::bail!("UUP feed down");
                    }
                    Ok(intraday_fixture(start, vec![]))
                }
            })
        };
        let first = run(false).await.unwrap();
        assert_eq!(calls.load(SeqCst), 8); // AAPL, SPY + 6 peers
        let peak_seen = peak.load(SeqCst);
        assert!(peak_seen > 1 && peak_seen <= CORR_CONCURRENCY, "peak in-flight {peak_seen}");
        // Re-open: every successful series is served from cache; the failed one is retried.
        let second = run(false).await.unwrap();
        assert_eq!(calls.load(SeqCst), 9);
        assert_eq!(uup.load(SeqCst), 2);
        assert_eq!(first.ols, second.ols);
        assert!(second.matrix.unavailable.iter().any(|(s, e)| s.starts_with("UUP") && e.contains("feed down")));
        // Reload bypasses the cache.
        run(true).await.unwrap();
        assert_eq!(calls.load(SeqCst), 17);
        assert_eq!(cache_ttl(Timeframe::M1), std::time::Duration::from_secs(60));
        assert_eq!(cache_ttl(Timeframe::H1), std::time::Duration::from_secs(600));
    }

    #[tokio::test]
    async fn vault_gap_is_reported_and_no_return_spans_it() {
        let history = history(tr_data::history::Archive::memory().unwrap());
        let bridge = tr_data::whats_news::Config { enabled: false, ..Default::default() }.bridge();
        let start = "2025-01-06T14:30:00Z".parse::<chrono::DateTime<chrono::Utc>>().unwrap().timestamp();
        let gap = (start + 2 * 1800, start + 3 * 1800);
        let r = research(&CorrCache::default(), false, &history, &bridge, false, "AAPL", "SPY 30m", |key, _| async move {
            let gaps = if tr_data::security(&key).ticker == "AAPL" { vec![gap] } else { vec![] };
            Ok(intraday_fixture(start, gaps))
        })
        .await
        .unwrap();
        assert_eq!(r.gaps, vec![gap]);
        assert_eq!(r.ols.n, 4); // 6 closed common prices → 5 returns, minus the gap pair.
        assert!(r.returns.iter().all(|p| !(p.start <= gap.0 && p.end >= gap.1)));
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| report_view(ui, 1, &r));
        output.textures_delta.clear();
        assert!(format!("{:?}", output.shapes).contains("UNVERIFIED JOIN"));
    }
    #[test]
    fn painter_renders_narrow_and_wide_without_gui() {
        let bars = test_bars(1_735_862_400, 86400);
        let r = maths::compute("AAPL", "SPY", &bars, &bars, 252.0, &[], "fixture").unwrap();
        for width in [280.0, 1440.0] {
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 1000.0))),
                    ..Default::default()
                },
                |ui| render(ui, &r),
            );
            assert!(!output.shapes.is_empty());
            output.textures_delta.clear();
        }
    }
}
