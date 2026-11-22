//! Relationship Studio pane and its archive/vault loading orchestration.
use super::{PaneKind, TerminalApp};
use crate::{congress_view::Action, dock::PaneId, theme, worker::Request};
use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, Ui};
use tr_core::{Bar, Timeframe};
use tr_data::studio_corr::{self as maths, CorrReport, NamedBars, RelateSpec};

pub struct CorrPane {
    pub symbol: String,
    pub spec: String,
    pub generation: u64,
    pub horizon: usize,
    pub loading: bool,
    pub error: Option<String>,
    pub report: Option<std::sync::Arc<CorrReport>>,
    /// Relate transform / fair-value inputs. `None` is legacy CORR / RBETA exactly.
    pub relate: Option<RelateSpec>,
    /// Next request bypasses the bar cache (set by the Reload button).
    fresh: bool,
    /// Save residual: the pending request token and the last message.
    pub save: super::studio_residual::SaveState,
}

impl CorrPane {
    pub fn new(symbol: impl Into<String>, spec: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            spec: spec.into(),
            generation: 0,
            horizon: 1,
            loading: false,
            error: None,
            report: None,
            relate: None,
            fresh: false,
            save: Default::default(),
        }
    }
    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        self.generation = crate::worker::studio_token();
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
        let benchmark = parse_spec(&self.spec).map(|(b, _)| b).unwrap_or_default();
        if let Some(r) = &mut self.relate {
            r.benchmark = benchmark;
        }
        worker.send(Request::StudioCorr {
            pane,
            generation: self.generation,
            symbol: tr_data::security(&self.symbol).key(),
            spec: self.spec.clone(),
            fresh: std::mem::take(&mut self.fresh),
            horizon: self.horizon,
            relate: self.relate.clone(),
        });
    }
    /// The accepted report + Relate fit when (and only when) it belongs to `symbol` and the current
    /// inputs. Returns whether the band fills are shown.
    pub fn fair_value_overlay(&self, symbol: &str) -> Option<(&CorrReport, &maths::RelateReport, bool)> {
        let current = self.relate.as_ref()?;
        let report = self.report.as_deref()?;
        let rel = report.relate.as_deref()?;
        let same = report.symbol == symbol
            && !self.loading
            && rel.fit.is_some()
            && rel.spec.transform == current.transform
            && rel.spec.beta_mode == current.beta_mode
            && rel.spec.covariance == current.covariance
            && rel.spec.matrix == current.matrix
            && rel.spec.benchmark == current.benchmark;
        same.then_some((report, rel, current.bands))
    }
    #[cfg(test)]
    pub fn accept(&mut self, generation: u64, result: Result<CorrReport, String>) {
        self.accept_shared(generation, result.map(std::sync::Arc::new));
    }
    pub fn accept_shared(&mut self, generation: u64, result: Result<std::sync::Arc<CorrReport>, String>) {
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
    observed_at: Option<i64>,
    receipt: Option<tr_data::source_receipt::SourceReceipt>,
}

/// Successful real-data loads keyed by (symbol, interval). Failures are never
/// cached, and nothing synthetic is ever stored. Re-opening CORR inside the TTL
/// reuses the bars; Reload bypasses the cache.
#[derive(Default)]
pub(crate) struct CorrCache {
    reports: std::sync::Arc<tr_data::report_cache::ReportCache>,
    generation: std::sync::atomic::AtomicU64,
    entries: std::sync::Mutex<std::collections::HashMap<(String, Timeframe), (std::time::Instant, Loaded)>>,
}
pub(crate) static CORR_CACHE: std::sync::LazyLock<CorrCache> = std::sync::LazyLock::new(CorrCache::default);

/// Daily archives change once a session; intraday entries live about one bar
/// (1–10 minutes), so a cached series is at most one closed bar behind.
pub(crate) fn cache_ttl(tf: Timeframe) -> std::time::Duration {
    std::time::Duration::from_secs(if tf == Timeframe::D1 { 900 } else { tf.seconds().clamp(60, 600) as u64 })
}

impl CorrCache {
    pub(crate) fn invalidate(&self) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        self.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        entries.clear();
    }

    fn get(&self, key: &str, tf: Timeframe) -> Option<Loaded> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(&(key.to_string(), tf))
            .filter(|(at, _)| at.elapsed() < cache_ttl(tf))
            .map(|(_, loaded)| loaded.clone())
    }
    fn put(&self, key: String, tf: Timeframe, loaded: Loaded, generation: u64) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if generation != self.generation.load(std::sync::atomic::Ordering::SeqCst) { return; }
        entries.retain(|(_, tf), (at, _)| at.elapsed() < cache_ttl(*tf));
        if entries.len() >= CACHE_CAPACITY
            && let Some(oldest) = entries.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone())
        {
            entries.remove(&oldest);
        }
        entries.insert((key, tf), (std::time::Instant::now(), loaded));
    }
}

/// Bounded concurrent loader shared by CORR and the Regression Lab: each name is read through the
/// shared (symbol, interval) bar cache (or fresh), admitted with `require_research_bars` on the
/// **full** supplied series, and only then trimmed to closed bars. A failed name stays a row with
/// its error; it is never substituted. Returns each row with its observation clock.
pub(crate) async fn load_rows<F, Fut>(
    cache: &CorrCache,
    fresh: bool,
    _history: &tr_data::history::Service,
    _bridge: &tr_data::whats_news::Bridge,
    names: Vec<String>,
    tf: Timeframe,
    load_intraday: F,
) -> Vec<(NamedBars, Option<i64>)>
where
    F: Fn(String, Timeframe) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<(Vec<Bar>, tr_data::vault::StitchInfo)>>,
{
    use futures::StreamExt;
    let load = |key: String| {
        let load_intraday = &load_intraday;
        async move {
            let generation = cache.generation.load(std::sync::atomic::Ordering::SeqCst);
            if !fresh && let Some(hit) = cache.get(&key, tf) {
                return (key, Ok(hit));
            }
            let result = load_intraday(key.clone(), tf).await.and_then(|(mut bars, info)| {
                tr_data::stats::require_research_bars(&bars, "Relationship research").map_err(anyhow::Error::msg)?;
                let n = tr_data::closed::closed_len(tf, &bars, tr_data::security(&key).class, info.fetched_at);
                bars.truncate(n);
                let source = if tf == Timeframe::D1 { info.source.clone() } else { info.label() };
                let receipt = info.receipt.clone().or_else(|| {
                    let mut r = tr_data::source_receipt::SourceReceipt::new(
                        &info.source,
                        bars.len(),
                        bars.last().map(|b| b.ts),
                    );
                    r.observed_at = info.fetched_at;
                    r.detail = source.clone();
                    (!r.provider_id.is_empty()).then_some(r)
                });
                Ok(Loaded { bars, source, gaps: info.gaps, observed_at: info.fetched_at, receipt })
            });
            if let Ok(loaded) = &result {
                cache.put(key.clone(), tf, loaded.clone(), generation);
            }
            (key, result)
        }
    };
    let loaded: Vec<(String, anyhow::Result<Loaded>)> =
        futures::stream::iter(names.into_iter().map(load)).buffered(CORR_CONCURRENCY).collect().await;
    loaded
        .into_iter()
        .map(|(key, result)| match result {
            Ok(Loaded { bars, source, gaps, observed_at, receipt }) => {
                (NamedBars { symbol: key, bars, source, error: None, gaps, receipt }, observed_at)
            }
            // A failed ticker is listed as unavailable; never substituted.
            Err(e) => (NamedBars { symbol: key, error: Some(format!("{e:#}")), ..Default::default() }, None),
        })
        .collect()
}

/// Kept here so the shared worker only registers a request/response arm. The supplied
/// loader is the existing vault-aware analysis_bars path (history_sourced underneath).
/// Symbols load concurrently (bounded by [`CORR_CONCURRENCY`]) through `cache`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn research_horizon<F, Fut>(
    cache: &CorrCache,
    fresh: bool,
    horizon: usize,
    relate: Option<RelateSpec>,
    jobs: Option<std::sync::Arc<crate::perf::JobLog>>,
    history: &tr_data::history::Service,
    bridge: &tr_data::whats_news::Bridge,
    sample: bool,
    symbol: &str,
    spec: &str,
    load_intraday: F,
) -> anyhow::Result<std::sync::Arc<CorrReport>>
where
    F: Fn(String, Timeframe) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<(Vec<Bar>, tr_data::vault::StitchInfo)>>,
{
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
    let loaded = load_rows(cache, fresh, history, bridge, names, tf, load_intraday).await;
    let observed_at = loaded.iter().filter_map(|(_, at)| *at).min();
    let rows: Vec<NamedBars> = loaded.into_iter().map(|(row, _)| row).collect();
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
    let reports = cache.reports.clone();
    // The parsed benchmark is authoritative; a caller's stale benchmark never keys the report.
    let relate = relate.map(|mut r| {
        r.benchmark = benchmark.clone();
        // The band toggle only shows or hides the overlay; it is not part of the computation key.
        r.bands = true;
        r
    });
    tokio::task::spawn_blocking(move || {
        let _job = jobs.as_ref().map(|j| crate::perf::JobTimer::new(j, "StudioCorr"));
        compute_cached(&reports, &a, &b, &rows, options, tf, &source, horizon, observed_at, relate.as_ref())
    })
    .await?
    .map_err(anyhow::Error::msg)
}

#[cfg(test)]
async fn research<F, Fut>(
    cache: &CorrCache,
    fresh: bool,
    history: &tr_data::history::Service,
    bridge: &tr_data::whats_news::Bridge,
    sample: bool,
    symbol: &str,
    spec: &str,
    load: F,
) -> anyhow::Result<std::sync::Arc<CorrReport>>
where
    F: Fn(String, Timeframe) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<(Vec<Bar>, tr_data::vault::StitchInfo)>>,
{
    research_horizon(cache, fresh, 1, None, None, history, bridge, sample, symbol, spec, load).await
}

/// Hash all admitted inputs and peer failures before cache lookup. Selection is
/// deliberately absent: each immutable report contains all four covariance modes.
fn compute_cached(
    reports: &tr_data::report_cache::ReportCache,
    a: &NamedBars,
    b: &NamedBars,
    rows: &[NamedBars],
    options: maths::Options,
    tf: Timeframe,
    source: &str,
    horizon: usize,
    observed_at: Option<i64>,
    relate: Option<&RelateSpec>,
) -> Result<std::sync::Arc<CorrReport>, String> {
    tr_data::stats::require_research_bars(&a.bars, "Relationship asset")?;
    tr_data::stats::require_research_bars(&b.bars, "Relationship benchmark")?;
    let mut h = tr_data::stable_hash::Fnv64::default();
    for row in rows {
        if row.error.is_none() {
            tr_data::stats::require_research_bars(&row.bars, "Relationship peer")?;
        }
        h.write_str(&row.symbol);
        h.write_str(&row.source);
        h.write_str(&format!("{:?} {:?} {:?}", row.gaps, row.error, Bar::provenance(&row.bars)));
        h.write_u64(tr_data::stable_hash::bars_digest(&row.bars));
    }
    h.write_str(&b.symbol);
    h.write_str(&b.source);
    h.write_str(&format!("{:?} {:?}", Bar::provenance(&a.bars), Bar::provenance(&b.bars)));
    h.write_u64(tr_data::stable_hash::bars_digest(&b.bars));
    // The observation clock is deliberately not keyed: for daily data it is the wall-clock load time,
    // which would make every reload a cache miss. Bar content, gaps and provenance are keyed.
    h.write_str(&format!("{:?} {:?} {:?} horizon={horizon}", options, a.gaps, b.gaps));
    // Relate keys everything above plus the full spec, the split, the grid convention and the
    // method schema, in a separate namespace so legacy keys and reports are untouched.
    if let Some(spec) = relate {
        h.write_str(&format!("{spec:?} train_frac=0.8 strict_grid=true {}", maths::METHOD_SCHEMA));
    }
    let mut key = tr_data::report_cache::ReportKey::new(
        if relate.is_some() { "studio_corr_v4" } else { "studio_corr_v2" },
        &a.symbol,
        tf,
        source,
        &a.bars,
        if relate.is_some() {
            "all covariance methods; 80/20 holdout; rolling60x1000; relate transforms + frozen fair value"
        } else {
            "all covariance methods; 80/20 holdout; rolling60x1000"
        },
    );
    key.params = h.finish();
    let digest = key.params;
    reports.get_or_compute(key, || {
        let mut r = maths::compute_series_at_horizon(a, b, rows, options, source, horizon)?;
        r.observed_at = observed_at;
        r.closed_thru = closed_thru(tf, tr_data::security(&a.symbol).class, r.as_of);
        if let Some(spec) = relate {
            // A Relate failure must not take the legacy CORR report down with it.
            r.relate = Some(Box::new(
                maths::compute_relate(a, b, rows, options, tf, spec, source, r.closed_thru, observed_at, digest)
                    .unwrap_or_else(|e| maths::RelateReport::failed(spec, digest, e, a, b)),
            ));
        }
        Ok(r)
    })
}

/// Match the existing closed_len end convention, including short cash buckets.
pub(crate) fn closed_thru(tf: Timeframe, class: tr_core::AssetClass, ts: i64) -> i64 {
    let equity = matches!(class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf | tr_core::AssetClass::Index);
    if !equity {
        return ts + tf.seconds();
    }
    let d = if tf == Timeframe::D1 {
        chrono::DateTime::from_timestamp(ts, 0).unwrap().date_naive()
    } else {
        tr_core::ny::civil(ts).date()
    };
    let close = tr_data::intraday::close_minute(d);
    let cash = tr_core::ny::to_utc(d.and_hms_opt(close / 60, close % 60, 0).unwrap());
    if tf == Timeframe::D1 {
        return cash;
    }
    let mut end = ts + tf.seconds();
    if tf == Timeframe::H4 && ts == tf.bucket_start(ts) {
        end = tr_data::intraday::regular_opens(d, Timeframe::H1)
            .into_iter()
            .filter(|&open| (ts..end).contains(&open))
            .last()
            .map(|open| open + 3600)
            .unwrap_or(end);
    }
    end.min(cash)
}


impl TerminalApp {
    pub(super) fn open_studio_corr(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        self.status = "Relationship Studio · common closed timestamps · verified data".into();
        self.open_panel(PaneKind::StudioCorr(Box::new(CorrPane::new(symbol, spec))));
    }
    pub(super) fn studio_corr_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("research pane");
        let PaneKind::StudioCorr(pane) = &mut self.panes[i].kind else { return vec![] };
        show(ui, id, pane, &self.worker, self.provider.eq_ignore_ascii_case("sample"), false)
    }
}

/// Shared renderer/action adapter for the standalone pane and Studio shell.
pub(super) fn show(
    ui: &mut Ui,
    id: PaneId,
    pane: &mut CorrPane,
    worker: &crate::worker::Worker,
    sample: bool,
    embedded: bool,
) -> Vec<Action> {
    let mut reload = false;
    let mut save: Option<(u64, std::sync::Arc<CorrReport>, Timeframe)> = None;
    ui.horizontal_wrapped(|ui| {
        super::studio_dist::studio_title(ui, "RELATIONSHIP STUDIO");
        if ui.small_button("Reload").on_hover_text("Re-read every series, bypassing the short bar cache").clicked() {
            reload = true;
            pane.fresh = true;
        }
        // Freezing needs the Relate fit; the embedded Studio shell has its own saved-model entry (STUDIO resid save).
        if !embedded {
            let fit = pane.report.as_ref().filter(|r| r.relate.as_ref().is_some_and(|x| x.fit.is_some())).cloned();
            let tf = parse_spec(&pane.spec).map(|(_, tf)| tf).ok();
            let can = !sample && pane.save.pending.is_none() && fit.is_some() && tf.is_some();
            if ui
                .add_enabled(can, egui::Button::new("Save residual").small())
                .on_hover_text("Freeze the Relate fit into an immutable residual model (STUDIO resid open <id>)")
                .clicked()
                && let (Some(r), Some(tf)) = (fit, tf)
            {
                let token = crate::worker::studio_token();
                pane.save = super::studio_residual::SaveState { pending: Some(token), msg: None };
                save = Some((token, r, tf));
            }
            if let Some(m) = &pane.save.msg {
                ui.label(RichText::new(m).font(theme::ui_font(10.0)).color(theme::muted()));
            }
        }
    });
    if let Some((token, report, tf)) = save {
        worker.send(crate::worker::Request::SaveStudioFit {
            pane: id,
            token,
            name: None,
            monitor: None,
            source: tr_data::studio_residual::FitSource::Relate { report, tf },
        });
    }
    if !embedded {
        ui.horizontal_wrapped(|ui| {
            ui.label("Asset");
            let a = ui.add(egui::TextEdit::singleline(&mut pane.symbol).desired_width(100.0).hint_text("AAPL"));
            ui.label("Benchmark / interval");
            let b = ui.add(egui::TextEdit::singleline(&mut pane.spec).desired_width(110.0).hint_text("SPY 1D"));
            reload |= (a.lost_focus() || b.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
            reload |= ui.small_button("Apply").clicked();
        });
    }
    ui.label(
        RichText::new("Log returns · common closed timestamps · no fill-forward · default SPY")
            .font(theme::ui_font(11.0))
            .color(theme::muted()),
    );
    if pane.relate.is_none() {
        ui.horizontal_wrapped(|ui| {
            ui.label("Return horizon");
            for h in [1, 5, 20] {
                if ui.selectable_label(pane.horizon == h, format!("{h} bars")).clicked() {
                    pane.horizon = h;
                    reload = true;
                }
            }
            if embedded {
                return;
            }
            if ui.small_button("Relate transforms").on_hover_text("Add Relate transforms and frozen-fit fair value").clicked() {
                let benchmark = parse_spec(&pane.spec).map(|(b, _)| b).unwrap_or_default();
                pane.relate = Some(RelateSpec::new(benchmark, tr_data::studio::Transform::LogReturn { h: pane.horizon }));
                reload = true;
            }
        });
    } else {
        reload |= relate_ui::controls(ui, pane);
    }
    if sample {
        pane.report = None;
        pane.loading = false;
        pane.error =
            Some("No real data in sample / demo display mode. Relationship Studio never uses sample prices.".into());
    } else if reload || (!pane.loading && pane.report.is_none() && pane.error.is_none()) {
        pane.request(id, worker);
    }
    super::studio_dist::studio_states(ui, pane.loading, pane.error.as_deref(), None);
    if let Some(r) = &pane.report {
        if r.relate.is_some() {
            egui::ScrollArea::vertical()
                .id_salt(("relate-scroll", id))
                .max_height(ui.available_height().min(300.0))
                .auto_shrink([false, true])
                .show(ui, |ui| relate_panel(ui, id, r));
        }
        ui.push_id(id, |ui| report_view(ui, id, r));
    }
    vec![]
}

pub(crate) mod relate_ui;
pub(crate) use relate_ui::fair_value_card;
#[cfg(test)]
pub(crate) use relate_ui::fit_card;

/// Dev/test fixture: a full CorrReport with its Relate report computed on the given daily bars.
/// `peers` extend the correlation matrix rows. Never used outside tests and the paint gate.
#[cfg(test)]
pub(super) fn relate_fixture_report(
    symbol: &str,
    spec: &RelateSpec,
    asset: &[Bar],
    benchmark: &[Bar],
    peers: &[NamedBars],
) -> CorrReport {
    let named = |symbol: &str, bars: &[Bar]| NamedBars {
        symbol: symbol.into(),
        bars: bars.to_vec(),
        source: "benchmark fixture only".into(),
        ..Default::default()
    };
    let (a, b) = (named(symbol, asset), named(&spec.benchmark, benchmark));
    let mut rows = vec![a.clone(), b.clone()];
    rows.extend(peers.iter().cloned());
    let options = maths::Options { annual_periods: 252., daily: true };
    let h = spec.transform.horizon();
    let mut r = maths::compute_series_at_horizon(&a, &b, &rows, options, "benchmark fixture only", h).unwrap();
    r.closed_thru = asset.last().unwrap().ts + 86_400;
    r.relate = Some(Box::new(
        maths::compute_relate(&a, &b, &rows, options, Timeframe::D1, spec, "benchmark fixture only", r.closed_thru, None, 1)
            .unwrap(),
    ));
    r
}

/// The Relate panel (fit tile, expanding β, common-panel matrix, receipts) for a report that has one.
pub(in crate::app) fn relate_panel(ui: &mut Ui, id: PaneId, r: &CorrReport) {
    if let Some(rel) = r.relate.as_deref() {
        relate_ui::panel(ui, id, r, rel);
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
pub(in crate::app) fn report_view(ui: &mut Ui, id: PaneId, r: &CorrReport) {
    super::studio_dist::gap_warning(ui, &r.gaps);
    egui::ScrollArea::vertical().id_salt(("studio-corr", id)).auto_shrink([false, false]).show(ui, |ui| render(ui, r));
}

fn render(ui: &mut Ui, r: &CorrReport) {
    let m = &r.ols;
    let selected = se_controls(ui, r);
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
    .on_hover_ui(|ui| {
        let parts = crate::widgets::chip_parts_opt(&[r.symbol.as_str(), r.benchmark.as_str()], &r.receipts);
        ui.label(crate::widgets::source_chip_detail(&parts, &r.source));
    });
    ui.label(
        RichText::new(format!(
            "{} common / {} asset / {} benchmark price bars",
            r.aligned_bars, r.asset_bars, r.benchmark_bars
        ))
        .color(theme::muted()),
    );
    {
        use crate::widgets::{MathCard, stat_tile_sized};
        let from = tr_data::stats::bar_date(r.path[0].ts);
        let to = tr_data::stats::bar_date(r.as_of);
        let base = |title: &str, formula: &str| {
            MathCard::new(title, formula)
                .n(m.n, "common returns")
                .span(from.clone(), to.clone())
                .provenance(r.provenance)
                .source(r.source.clone())
                .closed_thru(stamp(r.closed_thru))
        };
        let opt = |card: MathCard, v: Option<f64>| match v {
            Some(v) => card.value(format!("{v:.3}")),
            None => card.unavailable("zero variance or too few common returns"),
        };
        ui.horizontal_wrapped(|ui| {
            stat_tile_sized(ui, "BETA", &number(m.beta), m.beta.map_or_else(theme::muted, theme::sign), 22.0, || {
                opt(base("BETA", "β = Cov(rₐ, r_b) / Var(r_b)  (OLS slope)"), m.beta)
                    .input("β SE", number(m.beta_se))
                    .input("β t", number(m.beta_t))
            });
            let rho_ink = m.correlation.map_or(theme::muted(), theme::sign);
            stat_tile_sized(ui, "CORRELATION", &number(m.correlation), rho_ink, 22.0, || {
                opt(base("CORRELATION", "ρ = Cov(rₐ, r_b) / (σₐ σ_b)  (Pearson)"), m.correlation)
            });
            stat_tile_sized(ui, "R²", &number(m.r_squared), theme::text(), 22.0, || {
                opt(base("R²", "R² = 1 − SS_res / SS_tot  (= ρ² for one regressor)"), m.r_squared)
                    .input("ρ", number(m.correlation))
            });
            stat_tile_sized(
                ui,
                "ALPHA / YR",
                &percent(m.alpha_annual),
                m.alpha_annual.map_or_else(theme::muted, theme::sign),
                22.0,
                || {
                    let card = base("ALPHA / YR", "αₐ = α × P  (OLS intercept, annualised)")
                        .input("α SE", number(m.alpha_se))
                        .input("α t", number(m.alpha_t));
                    match m.alpha_annual {
                        Some(_) => card.value(percent(m.alpha_annual)),
                        None => card.unavailable("zero variance or too few common returns"),
                    }
                },
            );
        });
    }
    ui.label(
        RichText::new(format!(
            "Classical: α SE {}  t {}   ·   β SE {}  t {}   ·   Residual vol / yr {}",
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
             selected-horizon returns, classical (iid, n−2) errors retained above; selected covariance shown separately. No risk-free rate. \
             α / yr = per-bar α × {:.0} periods/year. Capture = mean asset ÷ mean benchmark log return on \
             benchmark up / down bars. Undefined values = —.",
            r.annual_periods
        ))
        .font(theme::ui_font(10.5))
        .color(theme::muted()),
    );
    // Prepared geometry is cached per report (pointer + content fingerprint) and per plot rect.
    let fp = fingerprint(r);
    let base = ui.id();
    let chart = |slot: u8| Chart { id: base.with(("corr-chart", slot)), fp };
    heading(ui, "RETURN CLOUD · CYAN = MOST RECENT 20");
    scatter_selected(ui, r, selected, chart(10));
    robust_rolling(ui, r, selected, chart(11));
    for correlation in [false, true] {
        heading(
            ui,
            if correlation {
                "ROLLING CORRELATION · 20 / 60 / 120 RETURNS"
            } else {
                "ROLLING BETA · 20 / 60 / 120 RETURNS"
            },
        );
        line_chart(
            ui,
            chart(correlation as u8),
            || {
                r.rolling
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
                    .collect()
            },
            false,
            true,
        );
    }
    heading(ui, "LOG PRICE RATIO · LEVEL");
    line_chart(
        ui,
        chart(2),
        || {
            vec![PlotLine {
                label: "ln(asset / benchmark)".into(),
                color: theme::cyan(),
                points: r.path.iter().map(|p| (p.ts as f64, Some(p.log_ratio))).collect(),
            }]
        },
        false,
        true,
    );
    heading(ui, "RATIO Z-SCORE · VS PRIOR 60 COMMON PRICES (CURRENT EXCLUDED)");
    line_chart(
        ui,
        chart(3),
        || {
            vec![PlotLine {
                label: "z60".into(),
                color: theme::amber(),
                points: r.path.iter().map(|p| (p.ts as f64, p.z60)).collect(),
            }]
        },
        false,
        true,
    );
    heading(ui, "IDIOSYNCRATIC CUMULATIVE RETURN");
    if r.horizon > 1 {
        ui.label(
            RichText::new(format!(
                "Unavailable at h={}: overlapping returns cannot be cumulated (each one-bar residual would be counted about {} times).",
                r.horizon, r.horizon
            ))
            .color(theme::muted()),
        );
    } else {
        line_chart(
            ui,
            chart(4),
            || {
                vec![PlotLine {
                    label: "exp(Σ OLS residual) − 1".into(),
                    color: theme::up(),
                    points: r.path.iter().map(|p| (p.ts as f64, p.residual_return)).collect(),
                }]
            },
            true,
            true,
        );
        ui.label(RichText::new("In-sample residuals use the full-sample fit and sum to approximately zero. This path is descriptive and uses future fit information; the ratio z-score is not a cointegration test. Missing timestamps can make a return span several bars; no return spans a vault gap.").font(theme::ui_font(10.5)).color(theme::muted()));
    }
    heading(ui, "CORRELATION MATRIX · PAIRWISE COMMON RETURNS");
    matrix(ui, &r.matrix);
}

/// Stable MathCard id: corr.coefficient.{alpha,beta}. All numbers here read the
/// accepted snapshot; covariance and testing never run on the interactive thread.
pub(in crate::app) fn inference_card(r: &CorrReport, selected: usize, coefficient: usize) -> crate::widgets::MathCard {
    use crate::widgets::MathCard;
    let fit = &r.robust.fits[selected];
    let estimate = [r.ols.alpha, r.ols.beta][coefficient];
    let interval = fit.intervals[coefficient].map(|(a, b)| format!("{a:+.6} … {b:+.6}")).unwrap_or_else(|| "—".into());
    let mut card=MathCard::new(if coefficient==0 {"CORR ALPHA · corr.coefficient.alpha"} else {"CORR BETA · corr.coefficient.beta"},
        "OLS θ=(XᵀX)⁻¹Xᵀy; V=A⁻¹SA⁻¹; CI=θ±1.96√Vjj; HAC S=Σggᵀ+Σwℓ(gglagᵀ+glaggᵀ); HC3 g=xe/(1−hii)")
        .n(r.ols.n,"common closed returns")
        .span(stamp(r.path[0].ts),stamp(r.as_of)).closed_thru(stamp(r.closed_thru))
        .source(&r.source).provenance(r.provenance)
        .input("SE method / bandwidth",fit.covariance.label())
        .input("observed source clock",r.observed_at.map(stamp).unwrap_or_else(||"closed input receipt; clock unavailable".into()))
        .input("95% pointwise CI",interval)
        .input("normal p / BY q",format!("{} / {:.6e}",number(fit.p[coefficient]),fit.q[coefficient]))
        .input("fixed family", "m=2: alpha=0 and beta=1; unavailable enters p=1")
        .note(format!("Span {} → {}; CLOSED THRU {}; {}",stamp(r.path[0].ts),stamp(r.as_of),stamp(r.closed_thru),r.source))
        .caveat("Robust inference is asymptotic and bandwidth-sensitive. Classical is iid with n−2 residual scaling. Irregular common endpoints are per observation, not per fixed elapsed minute. BY covers only these two tests per declared SE method; repeated exploration is outside the family.");
    if let Some(e) = &fit.covariance.unavailable {
        card = card.note(format!("Inference unavailable: {e}; interval and p unavailable, BY input p=1."));
    }
    match estimate {
        Some(v) => card.value(format!("{v:+.6}")),
        None => card.unavailable("singular benchmark or fewer than three returns"),
    }
}

fn coloured_label(ui: &mut Ui, parts: impl IntoIterator<Item = (String, Color32)>) -> egui::Response {
    let mut job = egui::text::LayoutJob::default();
    let font = egui::TextStyle::Body.resolve(ui.style());
    for (text, color) in parts {
        job.append(&text, 0., egui::TextFormat { font_id: font.clone(), color, ..Default::default() });
    }
    ui.label(job)
}

fn se_controls(ui: &mut Ui, r: &CorrReport) -> usize {
    use crate::widgets::{MathCardExt, stat_tile_sized};
    let id = ui.id().with("corr-se-method");
    let mut selected = ui.ctx().data(|d| d.get_temp::<usize>(id)).unwrap_or(2).min(3);
    if r.horizon > 1 && selected == 3 {
        selected = 2;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("SE");
        for (i, fit) in r.robust.fits.iter().enumerate() {
            if r.horizon > 1 && i == 3 {
                continue;
            }
            ui.selectable_value(&mut selected, i, fit.covariance.label());
        }
    });
    ui.ctx().data_mut(|d| d.insert_temp(id, selected));
    ui.label(format!(
        "CLOSED THRU {} · {}-observation returns · mean-response CI · alpha=0 / beta=1 family m=2",
        stamp(r.closed_thru),
        r.horizon
    ));
    if r.horizon > 1 {
        ui.label("Overlapping outcomes: default uniform h−1 kernel with labelled Bartlett fallback. Classical/HC3 omit serial overlap; the holdout purges the last h−1 training pairs that overlap the first test pair.");
    }
    if let Some(sen) = &r.robust.theil_sen {
        coloured_label(ui,[("Theil–Sen slope ".into(),theme::text()),(sen.slope.map(|v|format!("{v:+.4}")).unwrap_or_else(||"—".into()),sen.slope.map_or_else(theme::muted,theme::sign)),(format!(" · latest n={} · descriptive cross-check",sen.n),theme::text())]).math_card(|| {
            let card=crate::widgets::MathCard::new("CORR THEIL–SEN · corr.theil_sen","slope=median_i<j [(y[j]−y[i])/(x[j]−x[i])], x[j]≠x[i]")
                .n(sen.n,"latest common closed returns, capped at 2,000").source(&r.source).provenance(r.provenance)
                .span(stamp(r.returns[r.returns.len().saturating_sub(2000)].start),stamp(r.as_of)).closed_thru(stamp(r.closed_thru))
                .input("pairwise slopes",sen.slopes.to_string()).caveat("Descriptive cross-check only; no p-value or interval. Equal-x pairs are omitted; no causal or predictive interpretation.");
            match sen.slope {Some(v)=>card.value(format!("{v:+.5}")),None=>card.unavailable("no distinct benchmark returns")}
        });
    }
    if let Some(fit) = r.robust.fits.get(selected) {
        ui.horizontal_wrapped(|ui| {
            for (i, title) in ["ALPHA / OBS · selected SE", "BETA · selected SE"].iter().enumerate() {
                let v = [r.ols.alpha, r.ols.beta][i];
                let response = stat_tile_sized(
                    ui,
                    title,
                    &v.map(|v| format!("{v:+.5}")).unwrap_or_else(|| "—".into()),
                    v.map_or_else(theme::muted, theme::sign),
                    20.,
                    || inference_card(r, selected, i),
                );
                if i == 1 {
                    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("corr-probe"), response.rect));
                }
                ui.vertical(|ui| {
                    let parts = fit.intervals[i]
                        .map(|(a, b)| {
                            vec![
                                ("95% CI [".into(), theme::text()),
                                (format!("{a:+.5}"), theme::sign(a)),
                                (", ".into(), theme::text()),
                                (format!("{b:+.5}"), theme::sign(b)),
                                ("]".into(), theme::text()),
                            ]
                        })
                        .unwrap_or_else(|| vec![("95% CI —".into(), theme::muted())]);
                    coloured_label(ui, parts).math_card(|| inference_card(r, selected, i));
                    ui.label(format!(
                        "BY q={:.4} · {}",
                        fit.q[i],
                        if fit.p[i].is_none() {
                            "unavailable"
                        } else if fit.q[i] <= 0.10 {
                            "q ≤ 0.10"
                        } else {
                            "q > 0.10"
                        }
                    ));
                });
            }
        });
        if let Some(why) = &fit.covariance.unavailable {
            ui.label(RichText::new(format!("Selected inference unavailable: {why}")).color(theme::muted()));
        }
    }
    if let Some(h) = &r.robust.holdout {
        let card = || {
            let card=crate::widgets::MathCard::new("CORR HOLDOUT · corr.holdout.r2","R²_oos=1−Σtest(y−α_train−β_train x)²/Σtest(y−mean_train)²")
                .n(h.test_n,"test returns, frozen training coefficients")
                .span(stamp(r.returns.first().unwrap().start),stamp(r.as_of)).closed_thru(stamp(r.closed_thru))
                .source(&r.source).provenance(r.provenance)
                .input("train / purged / test",format!("{} / {} / {}; split {}",h.train_n,h.purged,h.test_n,stamp(h.split_ts)))
                .input("alpha / beta / train mean",format!("{} / {} / {}",number(h.alpha),number(h.beta),number(h.train_mean)))
                .caveat("Chronological first 80% fit, last 20% scored without refitting; negative R²_oos reads no skill. One split is descriptive, not a strategy test.");
            match h.r_squared_oos {
                Some(v) => card.value(format!("{v:+.4}")),
                None => card.unavailable(h.unavailable.as_deref().unwrap_or("unavailable")),
            }
        };
        coloured_label(
            ui,
            [
                (
                    if h.purged > 0 {
                        format!("HOLDOUT · train {} | purged {} | test {} · R²_oos ", h.train_n, h.purged, h.test_n)
                    } else {
                        format!("HOLDOUT · train {} | test {} · R²_oos ", h.train_n, h.test_n)
                    },
                    theme::text(),
                ),
                (
                    h.r_squared_oos.map(|v| format!("{v:+.4}")).unwrap_or_else(|| "—".into()),
                    h.r_squared_oos.map_or_else(theme::muted, theme::sign),
                ),
                (
                    if h.r_squared_oos.is_some_and(|r| r < 0.) { " · no skill".into() } else { String::new() },
                    theme::text(),
                ),
            ],
        )
        .math_card(card);
        let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(120.), 24.), Sense::hover());
        let split = rect.left() + rect.width() * (h.train_n + h.purged) as f32 / (h.train_n + h.purged + h.test_n).max(1) as f32;
        ui.painter().rect_filled(
            Rect::from_min_max(rect.min, egui::pos2(split, rect.bottom())),
            0.,
            theme::series(0).linear_multiply(0.15),
        );
        ui.painter().rect_filled(
            Rect::from_min_max(egui::pos2(split, rect.top()), rect.max),
            0.,
            theme::series(1).linear_multiply(0.15),
        );
        ui.painter().text(
            rect.left_center() + egui::vec2(5., 0.),
            egui::Align2::LEFT_CENTER,
            "TRAIN · frozen α / β",
            theme::mono(10.),
            theme::text(),
        );
        ui.painter().text(
            rect.right_center() - egui::vec2(5., 0.),
            egui::Align2::RIGHT_CENTER,
            "TEST",
            theme::mono(10.),
            theme::text(),
        );
        response.math_card(card);
        let (gauge, response) = ui.allocate_exact_size(egui::vec2(180., 18.), Sense::hover());
        let mid = gauge.center().x;
        ui.painter().hline(gauge.x_range(), gauge.center().y, Stroke::new(1., theme::border()));
        ui.painter().vline(mid, gauge.y_range(), Stroke::new(1., theme::muted()));
        if let Some(v) = h.r_squared_oos {
            let x = mid + v.clamp(-1., 1.) as f32 * gauge.width() / 2.;
            ui.painter().circle_filled(egui::pos2(x, gauge.center().y), 3., theme::series(1));
        }
        response.math_card(|| {
            card().note(
                "Gauge spans −1 to +1 with a zero tick; marker clips outside that span. The printed R² is unclipped.",
            )
        });
    }
    selected
}

fn quad(mesh: &mut egui::epaint::Mesh, points: [Pos2; 4], color: Color32) {
    let base = mesh.vertices.len() as u32;
    for pos in points {
        mesh.vertices.push(egui::epaint::Vertex { pos, uv: egui::epaint::WHITE_UV, color });
    }
    mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn segment(mesh: &mut egui::epaint::Mesh, a: Pos2, b: Pos2, width: f32, color: Color32) {
    let d = b - a;
    let length = d.length();
    if length > 0. {
        let normal = egui::vec2(-d.y, d.x) * (0.5 * width / length);
        quad(mesh, [a + normal, b + normal, b - normal, a - normal], color);
    }
}

/// One chart's identity for the paint caches: a stable widget id plus the report fingerprint.
#[derive(Clone, Copy)]
struct Chart {
    id: egui::Id,
    fp: u64,
}

/// Cheap identity of an accepted report. The address distinguishes live reports; the content
/// fields guard against an allocator reusing that address for a different report.
fn fingerprint(r: &CorrReport) -> u64 {
    mix(&(
        r as *const CorrReport as usize,
        r.as_of,
        r.closed_thru,
        r.horizon,
        r.returns.len(),
        r.path.len(),
        r.ols.beta.map(f64::to_bits),
        r.ols.alpha.map(f64::to_bits),
    ))
}
fn mix(v: &impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}
fn rect_bits(r: Rect) -> [u32; 4] {
    [r.min.x.to_bits(), r.min.y.to_bits(), r.max.x.to_bits(), r.max.y.to_bits()]
}

/// A single cached value per `id`, rebuilt only when `key` changes. Lives in egui temp data, so
/// it needs no cleanup and is dropped with the context.
fn cached<T: Send + Sync + 'static>(
    ctx: &egui::Context,
    id: egui::Id,
    key: u64,
    build: impl FnOnce() -> T,
) -> std::sync::Arc<T> {
    if let Some((k, v)) = ctx.data(|d| d.get_temp::<(u64, std::sync::Arc<T>)>(id))
        && k == key
    {
        return v;
    }
    let v = std::sync::Arc::new(build());
    ctx.data_mut(|d| d.insert_temp(id, (key, v.clone())));
    v
}

/// Screen-space geometry prepared for one (report, rect, selection): shared meshes plus the
/// screen positions used for nearest-point hover.
struct Prepared {
    meshes: Vec<std::sync::Arc<egui::epaint::Mesh>>,
    pos: Vec<Pos2>,
}

/// Report-level (rect independent) line data: lines, their finite points and bounds.
struct LineData {
    lines: Vec<PlotLine>,
    valid: Vec<Vec<(f64, f64)>>,
    x: (f64, f64),
    y: (f64, f64),
    any: bool,
}
impl LineData {
    fn new(lines: Vec<PlotLine>) -> Self {
        let valid: Vec<Vec<(f64, f64)>> =
            lines.iter().map(|l| l.points.iter().filter_map(|&(x, y)| y.map(|y| (x, y))).collect()).collect();
        let x = bounds(valid.iter().flatten().map(|p| p.0), false);
        let y = bounds(valid.iter().flatten().map(|p| p.1), true);
        let any = valid.iter().any(|v| !v.is_empty());
        Self { lines, valid, x, y, any }
    }
}

struct RollingData {
    x: (f64, f64),
    y: (f64, f64),
}

fn robust_rolling(ui: &mut Ui, r: &CorrReport, selected: usize, chart: Chart) {
    use crate::widgets::MathCardExt;
    let Some(points) = r.robust.rolling.get(selected).filter(|p| !p.is_empty()) else {
        return;
    };
    heading(ui, "ROLLING BETA · 60 RETURNS · POINTWISE 95% BAND · LAST 1,000 ENDPOINTS");
    let data = cached(ui.ctx(), chart.id.with("data"), mix(&(chart.fp, selected)), || RollingData {
        x: bounds(points.iter().map(|p| p.ts as f64), false),
        y: bounds(points.iter().flat_map(|p| p.interval.into_iter().flat_map(|(a, b)| [a, b]).chain(p.beta)), true),
    });
    let (x, y) = (data.x, data.y);
    let (plot, response) = axes(ui, x, y, false, true);
    if !ui.is_rect_visible(plot.rect) {
        return;
    }
    let painter = ui.painter_at(plot.rect);
    let prepared = cached(ui.ctx(), chart.id.with("mesh"), mix(&(chart.fp, selected, rect_bits(plot.rect))), || {
        let mut mesh = egui::epaint::Mesh::default();
        let mut stroke = egui::epaint::Mesh::default();
        for w in points.windows(2) {
            if w[1].break_before || w[0].interval.is_none() || w[1].interval.is_none() {
                continue;
            }
            if let (Some((a, b)), Some((c, d)), Some(ba), Some(bb)) =
                (w[0].interval, w[1].interval, w[0].beta, w[1].beta)
            {
                quad(
                    &mut mesh,
                    [
                        plot.pos(w[0].ts as f64, a),
                        plot.pos(w[1].ts as f64, c),
                        plot.pos(w[1].ts as f64, d),
                        plot.pos(w[0].ts as f64, b),
                    ],
                    theme::series(0).linear_multiply(0.15),
                );
                segment(&mut stroke, plot.pos(w[0].ts as f64, ba), plot.pos(w[1].ts as f64, bb), 1.3, theme::series(0));
            }
        }
        Prepared { meshes: vec![mesh.into(), stroke.into()], pos: vec![] }
    });
    for mesh in &prepared.meshes {
        painter.add(egui::Shape::Mesh(mesh.clone()));
    }
    let at = response
        .hover_pos()
        .map(|p| plot.x.0 + (p.x - plot.rect.left()) as f64 / plot.rect.width() as f64 * (plot.x.1 - plot.x.0))
        .unwrap_or(x.1);
    response.math_card(|| {
        let p=points.iter().min_by(|a,b|(a.ts as f64-at).abs().total_cmp(&(b.ts as f64-at).abs())).unwrap();
        let card=crate::widgets::MathCard::new("CORR ROLLING BETA · corr.rolling60","β_window=Cov(x,y)/Var(x); pointwise CI=β±1.96√Vββ")
            .n(60,"common closed returns per window").span(stamp(p.start),stamp(p.ts)).closed_thru(stamp(r.closed_thru))
            .source(&r.source).provenance(r.provenance)
            .input("window SE / bandwidth",&p.covariance_label)
            .input("95% pointwise CI",p.interval.map(|(a,b)|format!("{a:+.6} … {b:+.6}")).unwrap_or_else(||"—".into()))
            .note("Each window refits only its 60 past returns. Last 1,000 endpoints; pointwise descriptions, no significance badges. Any window crossing a vault join is unavailable; no band polygon crosses one.");
        match p.beta {Some(beta)=>card.value(format!("{beta:+.5}")),None=>card.unavailable("window crosses a vault join, singular design or insufficient observations")}
    });
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
    if !ui.is_rect_visible(outer) {
        return (p, response);
    }
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
fn line_chart(ui: &mut Ui, chart: Chart, build: impl FnOnce() -> Vec<PlotLine>, pct: bool, dates: bool) {
    let data = cached(ui.ctx(), chart.id.with("data"), chart.fp, || LineData::new(build()));
    if !data.any {
        ui.label(RichText::new("Not enough common observations, or zero variance.").color(theme::muted()));
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for line in &data.lines {
            ui.label(RichText::new(&line.label).font(theme::mono(10.0)).color(line.color));
        }
    });
    let (plot, response) = axes(ui, data.x, data.y, pct, dates);
    if !ui.is_rect_visible(plot.rect) {
        return;
    }
    let painter = ui.painter_at(plot.rect);
    let prepared = cached(ui.ctx(), chart.id.with("mesh"), mix(&(chart.fp, rect_bits(plot.rect))), || {
        let meshes = data
            .lines
            .iter()
            .map(|line| {
                let mut mesh = egui::epaint::Mesh::default();
                mesh.vertices.reserve(line.points.len() * 4);
                mesh.indices.reserve(line.points.len() * 6);
                for w in line.points.windows(2) {
                    if let (Some(a), Some(b)) = (w[0].1, w[1].1) {
                        segment(&mut mesh, plot.pos(w[0].0, a), plot.pos(w[1].0, b), 1.6, line.color);
                    }
                }
                mesh.into()
            })
            .collect();
        Prepared { meshes, pos: vec![] }
    });
    for mesh in &prepared.meshes {
        painter.add(egui::Shape::Mesh(mesh.clone()));
    }
    if let Some(pos) = response.hover_pos().filter(|p| plot.rect.contains(*p)) {
        let x = plot.x.0 + (pos.x - plot.rect.left()) as f64 / plot.rect.width() as f64 * (plot.x.1 - plot.x.0);
        let mut labels = Vec::new();
        for (line, valid) in data.lines.iter().zip(&data.valid) {
            // Nearest finite point by timestamp (points are in time order).
            let at = valid.partition_point(|p| p.0 < x);
            let nearest = [at.checked_sub(1), (at < valid.len()).then_some(at)]
                .into_iter()
                .flatten()
                .map(|i| valid[i])
                .min_by(|a, b| (a.0 - x).abs().total_cmp(&(b.0 - x).abs()));
            if let Some((ts, v)) = nearest {
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
fn scatter_selected(ui: &mut Ui, r: &CorrReport, selected: usize, chart: Chart) {
    let data = cached(ui.ctx(), chart.id.with("data"), mix(&(chart.fp, selected)), || {
        let x = bounds(r.returns.iter().map(|p| p.benchmark), true);
        let mut ys: Vec<_> = r.returns.iter().map(|p| p.asset).collect();
        if let (Some(a), Some(b), Some(fit)) = (r.ols.alpha, r.ols.beta, r.robust.fits.get(selected)) {
            for at in [x.0, x.1] {
                if let Some(se) = fit.covariance.mean_se(&[1., at]) {
                    ys.extend([a + b * at - 1.96 * se, a + b * at + 1.96 * se]);
                }
            }
        }
        RollingData { x, y: bounds(ys.into_iter(), true) }
    });
    let fit = r.robust.fits.get(selected);
    let x = data.x;
    let (plot, response) = axes(ui, x, data.y, true, false);
    if !ui.is_rect_visible(plot.rect) {
        scatter_caption(ui, r, selected);
        return;
    }
    let painter = ui.painter_at(plot.rect);
    if let (Some(a), Some(b)) = (r.ols.alpha, r.ols.beta) {
        // Independent convex strips along the x-axis. This fan is a function
        // of benchmark return, never a time polygon linking across a vault gap.
        for k in 0..64 {
            let lo = x.0 + (x.1 - x.0) * k as f64 / 64.;
            let hi = x.0 + (x.1 - x.0) * (k + 1) as f64 / 64.;
            if let Some((sl, sh)) =
                fit.and_then(|f| f.covariance.mean_se(&[1., lo]).zip(f.covariance.mean_se(&[1., hi])))
            {
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        plot.pos(lo, a + b * lo - 1.96 * sl),
                        plot.pos(hi, a + b * hi - 1.96 * sh),
                        plot.pos(hi, a + b * hi + 1.96 * sh),
                        plot.pos(lo, a + b * lo + 1.96 * sl),
                    ],
                    theme::series(0).linear_multiply(0.16),
                    Stroke::NONE,
                ));
            }
        }
        painter
            .line_segment([plot.pos(x.0, a + b * x.0), plot.pos(x.1, a + b * x.1)], Stroke::new(1.7, theme::series(0)));
    }
    let prepared = cached(ui.ctx(), chart.id.with("mesh"), mix(&(chart.fp, selected, rect_bits(plot.rect))), || {
        let mut cloud = egui::epaint::Mesh::default();
        cloud.vertices.reserve(r.returns.len() * 4);
        cloud.indices.reserve(r.returns.len() * 6);
        let mut pos = Vec::with_capacity(r.returns.len());
        for (i, p) in r.returns.iter().enumerate() {
            let point = plot.pos(p.benchmark, p.asset);
            pos.push(point);
            if i < r.returns.len().saturating_sub(20) {
                cloud.add_colored_rect(
                    Rect::from_center_size(point, egui::vec2(3., 3.)),
                    theme::muted().linear_multiply(0.45),
                );
            }
        }
        Prepared { meshes: vec![cloud.into()], pos }
    });
    for mesh in &prepared.meshes {
        painter.add(egui::Shape::Mesh(mesh.clone()));
    }
    for &point in prepared.pos.iter().rev().take(20) {
        painter.circle_filled(point, 3., theme::cyan());
    }
    if let Some(pos) = response.hover_pos().filter(|p| plot.rect.contains(*p))
        && let Some((i, point)) = prepared
            .pos
            .iter()
            .copied()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.distance_sq(pos).total_cmp(&b.distance_sq(pos)))
    {
        let p = &r.returns[i];
        painter.circle_stroke(point, 5.0, Stroke::new(1.5, theme::amber()));
        response.on_hover_text(format!(
            "{} → {}\nBenchmark x: {}\nAsset y: {}",
            stamp(p.start),
            stamp(p.end),
            percent(Some(p.benchmark)),
            percent(Some(p.asset))
        ));
    }
    scatter_caption(ui, r, selected);
}
fn scatter_caption(ui: &mut Ui, r: &CorrReport, selected: usize) {
    use crate::widgets::MathCardExt;
    ui.label(
        RichText::new("x: benchmark log return · y: asset log return · mean-response CI (95%, pointwise)")
            .font(theme::ui_font(10.5))
            .color(theme::muted()),
    )
    .math_card(|| {
        inference_card(r, selected, 0)
            .note("Fan: ŷ(x) ± 1.96 √((1,x)V(1,x)ᵀ). Mean response, not a prediction interval.")
    });
}

fn two_dp(x: f64) -> String {
    format!("{x:.2}")
}
/// Fixed ρ heat scale for the matrix (never auto-scaled).
fn rho_scale() -> crate::widgets::HeatScale {
    crate::widgets::HeatScale::diverging(1.0, "ρ", two_dp)
}
fn matrix(ui: &mut Ui, m: &maths::Matrix) {
    matrix_impl(ui, m, false);
}
/// `common`: every cell is one complete-case set (Relate common panel, id `corr.matrix.common`).
fn matrix_impl(ui: &mut Ui, m: &maths::Matrix, common: bool) {
    use crate::widgets::{MathCard, MathCardExt};
    let hs = rho_scale();
    hs.key(ui);
    egui::ScrollArea::horizontal().id_salt(("corr-matrix", common)).show(ui, |ui| {
        egui::Grid::new(("corr-matrix-grid", common)).spacing(egui::vec2(3.0, 3.0)).show(ui, |ui| {
            ui.label("");
            for name in &m.symbols {
                ui.label(RichText::new(tr_data::security(name).ticker).font(theme::mono(10.0)).color(theme::muted()));
            }
            ui.end_row();
            for (i, row) in m.cells.iter().enumerate() {
                ui.label(RichText::new(tr_data::security(&m.symbols[i]).ticker).font(theme::mono(10.0)));
                for (j, cell) in row.iter().enumerate() {
                    let rho = cell.correlation.filter(|v| v.is_finite());
                    let ink = rho.map_or(theme::muted(), |v| hs.ink(v));
                    let label = format!("{}\nn={}", rho.map(two_dp).unwrap_or_else(|| "—".into()), cell.n);
                    egui::Frame::new()
                        .fill(hs.fill_gated(rho, true))
                        .inner_margin(5.0)
                        .show(ui, |ui| ui.label(RichText::new(label).font(theme::mono(10.0)).color(ink)))
                        .inner
                        .math_card(|| {
                            let pair = format!(
                                "{} / {}",
                                tr_data::security(&m.symbols[i]).ticker,
                                tr_data::security(&m.symbols[j]).ticker
                            );
                            let card = MathCard::new(
                                if common { format!("ρ {pair} · corr.matrix.common") } else { format!("ρ {pair}") },
                                "ρ = Cov(rₓ, r_y) / (σₓ σ_y)  (Pearson, log returns)",
                            )
                            .n(cell.n, "common returns")
                            .note(if common {
                                "Common panel: one complete-case set, strict endpoints in every row; every cell has the same n."
                            } else {
                                "Pairwise common endpoints; sample size may differ by pair."
                            });
                            match (rho, &cell.reason) {
                                (Some(v), _) => card.value(two_dp(v)),
                                (None, Some(why)) => card.unavailable(why.clone()),
                                (None, None) => card.unavailable("not computed"),
                            }
                        });
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
        let s = PaneSettings::StudioCorr {
            symbol: "AAPL US Equity".into(),
            spec: "QQQ 1H".into(),
            horizon: 5,
            relate: Some(RelateSpec::new("QQQ US Equity", tr_data::studio::Transform::LogReturn { h: 5 })),
        };
        let serialized = serde_json::to_string(&s).unwrap();
        let restored = PaneKind::from_settings(serde_json::from_str(&serialized).unwrap());
        assert_eq!(serde_json::to_value(restored.settings()).unwrap(), serde_json::to_value(s).unwrap());
        let PaneKind::StudioCorr(p) = restored else { panic!("relationship kind") };
        assert_eq!(p.spec, "QQQ 1H");
        assert_eq!(p.generation, 0);
        assert!(p.report.is_none());
        assert_eq!(p.horizon, 5);
        assert!(p.relate.is_some());
        let old: PaneSettings = serde_json::from_str(r#"{"StudioCorr":{}}"#).unwrap();
        assert_eq!(
            old,
            PaneSettings::StudioCorr { symbol: String::new(), spec: String::new(), horizon: 1, relate: None }
        );
        let PaneKind::StudioCorr(p) = PaneKind::from_settings(old) else { panic!("relationship kind") };
        assert_eq!((p.horizon, p.relate.is_none()), (1, true));
    }
    #[test]
    fn restore_normalises_a_hand_edited_relate_horizon_and_old_specs_load() {
        let edited = r#"{"StudioCorr":{"symbol":"AAPL US Equity","horizon":7,"relate":{"benchmark":"QQQ US Equity","transform":{"LogReturn":{"h":7}}}}}"#;
        let PaneKind::StudioCorr(p) = PaneKind::from_settings(serde_json::from_str(edited).unwrap()) else {
            panic!("relationship kind")
        };
        assert_eq!(p.horizon, 1);
        let spec = p.relate.unwrap();
        assert_eq!(spec.transform, tr_data::studio::Transform::LogReturn { h: 1 });
        assert!(spec.bands);
        let kept = r#"{"StudioCorr":{"relate":{"benchmark":"QQQ US Equity","transform":{"Change":{"h":20}}}}}"#;
        let PaneKind::StudioCorr(p) = PaneKind::from_settings(serde_json::from_str(kept).unwrap()) else { panic!() };
        assert_eq!(p.relate.unwrap().transform, tr_data::studio::Transform::Change { h: 20 });
    }
    #[test]
    fn relate_words_bands_off_keeps_report_and_h_on_level_explains() {
        let mut app = crate::app::tests::test_app();
        app.open_relate(Some("AAPL"), "");
        app.open_relate(None, "transform=logreturn h=5");
        let focus = app.focus;
        let report = |app: &mut crate::app::TerminalApp| {
            let PaneKind::Studio(s) = &mut app.panes.iter_mut().find(|p| p.id == focus).unwrap().kind else { panic!() };
            s.corr_mut().map(|p| (p.report.is_some(), p.relate.clone()))
        };
        let bars: Vec<_> = (0..5)
            .map(|i| {
                let p = 100.0 + i as f64;
                Bar::new(i * 86400, p, p, p, p, 1.0).with_provenance(tr_core::Provenance::Real)
            })
            .collect();
        {
            let PaneKind::Studio(s) = &mut app.panes.iter_mut().find(|p| p.id == focus).unwrap().kind else { panic!() };
            let p = s.corr_mut().unwrap();
            p.report = Some(std::sync::Arc::new(maths::compute("A", "B", &bars, &bars, 252.0, &[], "unit").unwrap()));
        }
        app.open_relate(None, "bands=off");
        let (kept, spec) = report(&mut app).unwrap();
        assert!(kept, "bands=off must not clear the report");
        assert!(!spec.unwrap().bands);
        app.open_relate(None, "transform=level");
        assert!(!report(&mut app).unwrap().0, "a transform change recomputes");
        app.open_relate(None, "h=5");
        assert!(app.status.contains("only to Δ"), "{}", app.status);
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
    async fn daily_uses_worker_resolver_and_sample_empty_state() {
        let start = 1_735_862_400; // 2025-01-03 00:00 UTC
        let mut archive = tr_data::history::Archive::memory().unwrap();
        for ticker in ["AAPL", "SPY"] {
            archive.import_verified(ticker, ticker, &test_bars(start, 86400)).unwrap();
        }
        let history = history(archive);
        let bridge = tr_data::whats_news::Config { enabled: false, ..Default::default() }.bridge();
        let cache = CorrCache::default();
        let r = research(&cache, false, &history, &bridge, false, "AAPL", "", |key, tf| {
            let history = &history;
            async move {
                assert_eq!(tf, Timeframe::D1);
                let ticker = tr_data::whats_news::archive_ticker(&key);
                let bars = history.db(move |db| db.daily(&ticker)).await?;
                Ok((bars, tr_data::vault::StitchInfo { source: "Local research archive".into(), fetched_at: Some(chrono::Utc::now().timestamp()), ..Default::default() }))
            }
        })
        .await
        .unwrap();
        assert_eq!(r.ols.n, 5);
        assert!(r.source.contains("Local research archive"));
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
            anyhow::bail!("no history: worker resolver exhausted sources")
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
        let r =
            research(&CorrCache::default(), false, &history, &bridge, false, "AAPL", "SPY 30m", |key, tf| async move {
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
        let r =
            research(&CorrCache::default(), false, &history, &bridge, false, "AAPL", "SPY 30m", |key, _| async move {
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
    fn big_meshes(out: &egui::FullOutput) -> Vec<std::sync::Arc<egui::epaint::Mesh>> {
        out.shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Mesh(m) if m.vertices.len() > 500 => Some(m.clone()),
                _ => None,
            })
            .collect()
    }
    fn corr_text(out: &mut egui::FullOutput) -> String {
        out.textures_delta.clear();
        out.shapes
            .iter()
            .filter_map(|s| if let egui::Shape::Text(t) = &s.shape { Some(t.galley.job.text.clone()) } else { None })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn corr_paint_reuses_prepared_meshes() {
        let (a, b) = tr_perf::fixtures::relationship(400);
        let r = maths::compute("AAPL", "QQQ", &a, &b, 252., &[], "fixture").unwrap();
        let ctx = egui::Context::default();
        let frame = |ctx: &egui::Context, r: &CorrReport, width: f32, se: usize| {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 3000.0))),
                    ..Default::default()
                },
                |ui| {
                    ui.ctx().data_mut(|d| d.insert_temp(ui.id().with("corr-se-method"), se));
                    render(ui, r)
                },
            );
            out.textures_delta.clear();
            big_meshes(&out)
        };
        for _ in 0..3 {
            frame(&ctx, &r, 1200., 2);
        }
        let (one, two) = (frame(&ctx, &r, 1200., 2), frame(&ctx, &r, 1200., 2));
        // cloud, rolling band + stroke, 3 rolling lines × 2 charts, ratio, z60, residual
        assert!(one.len() >= 8, "expected cached meshes, got {}", one.len());
        assert_eq!(one.len(), two.len());
        assert!(one.iter().zip(&two).all(|(a, b)| std::sync::Arc::ptr_eq(a, b)), "identical frames must reuse meshes");
        // A different selected SE rebuilds the SE-dependent meshes (cloud / rolling band).
        let sel = frame(&ctx, &r, 1200., 1);
        assert!(sel.iter().zip(&two).any(|(a, b)| !std::sync::Arc::ptr_eq(a, b)), "SE change must rebuild");
        // A different width rebuilds every rect-dependent mesh.
        let ctx2 = egui::Context::default();
        for _ in 0..3 {
            frame(&ctx2, &r, 1200., 2);
        }
        let before = frame(&ctx2, &r, 1200., 2);
        let wide = frame(&ctx2, &r, 900., 2);
        assert_eq!(before.len(), wide.len());
        assert!(before.iter().zip(&wide).all(|(a, b)| !std::sync::Arc::ptr_eq(a, b)), "width change must rebuild");
        // A different report (new address and content) never reuses meshes.
        let other = maths::compute("AAPL", "QQQ", &b, &a, 252., &[], "fixture").unwrap();
        let rebuilt = frame(&ctx2, &other, 900., 2);
        assert!(rebuilt.iter().zip(&wide).all(|(a, b)| !std::sync::Arc::ptr_eq(a, b)));
    }
    #[test]
    fn corr_residual_path_unavailable_for_overlapping_h() {
        let (a, b) = tr_perf::fixtures::relationship(300);
        let (named_a, named_b) = (
            NamedBars { symbol: "A".into(), bars: a, ..Default::default() },
            NamedBars { symbol: "B".into(), bars: b, ..Default::default() },
        );
        let opts = maths::Options { annual_periods: 252., daily: true };
        for h in [5, 20] {
            let r = maths::compute_series_at_horizon(&named_a, &named_b, &[], opts, "fixture", h).unwrap();
            assert!(r.path.iter().all(|p| p.residual_return.is_none()), "h={h}");
            let ctx = egui::Context::default();
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200., 3000.))),
                    ..Default::default()
                },
                |ui| render(ui, &r),
            );
            let text = corr_text(&mut out);
            assert!(text.contains("overlapping returns cannot be cumulated"), "h={h}: {text}");
            assert!(!text.contains("exp(Σ OLS residual)"));
        }
        let r = maths::compute_series_at_horizon(&named_a, &named_b, &[], opts, "fixture", 1).unwrap();
        assert!(r.path.iter().skip(1).any(|p| p.residual_return.is_some()), "h=1 path unchanged");
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200., 3000.))),
                ..Default::default()
            },
            |ui| render(ui, &r),
        );
        assert!(corr_text(&mut out).contains("exp(Σ OLS residual)"));
    }
    #[test]
    fn corr_matrix_paints_rho_key_and_keyed_fills() {
        let cell =
            |r: Option<f64>, n| maths::MatrixCell { correlation: r, n, reason: r.is_none().then(|| "n<20".into()) };
        let m = maths::Matrix {
            symbols: vec!["AAPL".into(), "SPY".into()],
            cells: vec![
                vec![cell(Some(1.0), 250), cell(Some(-0.45), 240)],
                vec![cell(Some(-0.45), 240), cell(None, 3)],
            ],
            unavailable: vec![],
        };
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| matrix(ui, &m));
        out.textures_delta.clear();
        let texts: Vec<String> = out
            .shapes
            .iter()
            .filter_map(|s| if let egui::Shape::Text(t) = &s.shape { Some(t.galley.job.text.clone()) } else { None })
            .collect();
        let hs = rho_scale();
        assert_eq!(hs.entries().map(|e| e.1), ["≤−1.00", "−1.00…−0.20", "±0.20", "+0.20…+1.00", "≥+1.00"]);
        for (_, label) in hs.entries() {
            assert!(texts.contains(&label), "{label}: {texts:?}");
        }
        let fills: Vec<_> = out
            .shapes
            .iter()
            .filter_map(|s| if let egui::Shape::Rect(r) = &s.shape { Some(r.fill) } else { None })
            .collect();
        assert!(fills.contains(&hs.fill(-0.45)) && fills.contains(&hs.fill(1.0)) && fills.contains(&theme::control()));
        // Correlation is never printed in the sign colours on the matrix.
        assert!(!fills.contains(&theme::up()) && !fills.contains(&theme::down()));
    }
}

#[cfg(test)]
mod cache_v2_tests {
    #[test]
    fn archive_invalidation_rejects_inflight_old_generation() {
        let cache = super::CorrCache::default();
        let row = super::Loaded { bars: vec![], source: "archive".into(), gaps: vec![], observed_at: None, receipt: None };
        cache.put("AAPL".into(), Timeframe::D1, row.clone(), 0);
        assert!(cache.get("AAPL", Timeframe::D1).is_some());
        cache.invalidate();
        assert!(cache.get("AAPL", Timeframe::D1).is_none());
        cache.put("AAPL".into(), Timeframe::D1, row.clone(), 0);
        assert!(cache.get("AAPL", Timeframe::D1).is_none());
        cache.put("AAPL".into(), Timeframe::D1, row, 1);
        assert!(cache.get("AAPL", Timeframe::D1).is_some());
    }

    use super::*;
    #[test]
    fn report_cache_identity_corrections_peer_failures_clocks_gaps_horizons() {
        let reports = tr_data::report_cache::ReportCache::new(16);
        let (a, b) = tr_perf::fixtures::relationship(80);
        let a = NamedBars { symbol: "A".into(), bars: a, source: "verified fixture".into(), ..Default::default() };
        let b = NamedBars { symbol: "B".into(), bars: b, source: "verified fixture".into(), ..Default::default() };
        let options = maths::Options { annual_periods: 252., daily: true };
        let call = |a: &NamedBars, b: &NamedBars, rows: &[NamedBars], h, observed| {
            compute_cached(&reports, a, b, rows, options, Timeframe::D1, "test", h, observed, None).unwrap()
        };
        let first = call(&a, &b, &[], 1, Some(100));
        let same = call(&a, &b, &[], 1, Some(100));
        assert!(std::sync::Arc::ptr_eq(&first, &same));
        let mut corrected = a.clone();
        corrected.bars[2].close += 0.001;
        assert!(!std::sync::Arc::ptr_eq(&first, &call(&corrected, &b, &[], 1, Some(100))));
        let mut gap = a.clone();
        gap.gaps.push((a.bars[20].ts, a.bars[21].ts));
        assert!(!std::sync::Arc::ptr_eq(&first, &call(&gap, &b, &[], 1, Some(100))));
        assert!(!std::sync::Arc::ptr_eq(&first, &call(&a, &b, &[], 5, Some(100))));
        // Slice-5 correction: the wall-clock observation time is not part of the key, so a reload
        // with identical bars reuses the report (the original assertion expected a miss).
        assert!(std::sync::Arc::ptr_eq(&first, &call(&a, &b, &[], 1, Some(101))));
        let peer = NamedBars { symbol: "SPY".into(), error: Some("missing".into()), ..Default::default() };
        let with_peer = call(&a, &b, &[peer], 1, Some(100));
        assert!(!std::sync::Arc::ptr_eq(&first, &with_peer));
        let mut bad = a.clone();
        bad.bars.last_mut().unwrap().provenance = tr_core::Provenance::Sample;
        assert!(compute_cached(&reports, &bad, &b, &[], options, Timeframe::D1, "test", 1, Some(100), None).is_err());
    }

    #[test]
    fn relate_failure_keeps_the_legacy_corr_report_with_the_reason() {
        let reports = tr_data::report_cache::ReportCache::new(16);
        let (a, b) = tr_perf::fixtures::relationship(80);
        let a = NamedBars { symbol: "A".into(), bars: a, source: "verified fixture".into(), ..Default::default() };
        let b = NamedBars { symbol: "B".into(), bars: b, source: "verified fixture".into(), ..Default::default() };
        let options = maths::Options { annual_periods: 252., daily: true };
        // h=7 is unsupported, so compute_relate itself errors.
        let spec = RelateSpec::new("B", tr_data::studio::Transform::LogReturn { h: 7 });
        let r = compute_cached(&reports, &a, &b, &[], options, Timeframe::D1, "test", 1, Some(100), Some(&spec))
            .expect("a Relate error must not fail the CORR report");
        assert!(!r.returns.is_empty() || r.as_of > 0);
        let rel = r.relate.as_deref().expect("relate section present");
        assert!(rel.fit.is_none() && rel.points.is_empty());
        assert!(rel.unavailable.as_deref().is_some_and(|e| e.contains("h must be")), "{:?}", rel.unavailable);
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    fn ts(s: &str) -> i64 {
        s.parse::<chrono::DateTime<chrono::Utc>>().unwrap().timestamp()
    }
    #[test]
    fn closed_thru_respects_short_cash_buckets_and_crypto_days() {
        assert_eq!(
            closed_thru(Timeframe::H1, tr_core::AssetClass::Equity, ts("2026-11-27T17:30:00Z")),
            ts("2026-11-27T18:00:00Z")
        );
        assert_eq!(
            closed_thru(Timeframe::D1, tr_core::AssetClass::Equity, ts("2026-11-27T00:00:00Z")),
            ts("2026-11-27T18:00:00Z")
        );
        assert_eq!(
            closed_thru(Timeframe::D1, tr_core::AssetClass::Crypto, ts("2026-11-27T00:00:00Z")),
            ts("2026-11-28T00:00:00Z")
        );
    }
}
