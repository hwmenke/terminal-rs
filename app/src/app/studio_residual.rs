//! Frozen residual tickers (Studio slice 6): the RESID pane. A saved Lab / Relate fit is loaded from
//! disk (immutable), scored on freshly re-admitted live data and checked (ADF, fixed-horizon CUSUM
//! monitor, half-life). The maths lives in `tr_data::studio_residual`; this module loads real verified
//! rows through the shared bounded loader, keys the report, and paints immutable `Arc`s.
//! Nothing here fits: a refit is a new Lab fit saved as a new version.
use super::studio_corr::{CorrCache, load_rows, relate_ui::signed};
use super::studio_dist::{stamp, studio_heading, studio_title};
use super::{PaneKind, TerminalApp};
use crate::studio_models::{ModelListing, ModelUnavailable};
use crate::widgets::{MathCard, MathCardExt, stat_tile};
use crate::{congress_view::Action, dock::PaneId, theme, worker::Request};
use egui::{Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use std::sync::{Arc, LazyLock};
use tr_core::{Bar, Timeframe};
use tr_data::studio_corr::NamedBars;
use tr_data::studio_lab::LabSpec;
use tr_data::studio_residual::{
    self as resid, CheckState, FitSource, FrozenFit, InputCheck, MonitorResult, ResidArgs, ResidSegment,
    ResidualChecks, ResidualReport,
};

/// Reports and checks are small (≤ 5,000 points; m-length vectors); keep the 8 most recent of each.
static RESID_REPORTS: LazyLock<tr_data::report_cache::ReportCache> =
    LazyLock::new(|| tr_data::report_cache::ReportCache::new(8));
static RESID_CHECKS: LazyLock<tr_data::report_cache::ReportCache> =
    LazyLock::new(|| tr_data::report_cache::ReportCache::new(8));

/// Save-button state shared by the Lab and CORR panes: the token of the pending request and the last
/// message. Only a response echoing `pending` is accepted.
#[derive(Default, Debug)]
pub struct SaveState {
    pub pending: Option<u64>,
    pub msg: Option<String>,
}

// ---- worker side ---------------------------------------------------------------------------------

/// Loads the saved target and every saved factor through the shared bounded loader (full-source
/// admission before the closed trim), then scores the frozen model. A failed or unadmitted factor
/// stays in the report as a `factor_error`; it is never dropped, substituted or re-estimated.
/// Sample mode makes no provider calls.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn score_research<F, Fut>(
    cache: &CorrCache,
    fresh: bool,
    jobs: Option<Arc<crate::perf::JobLog>>,
    history: &tr_data::history::Service,
    bridge: &tr_data::whats_news::Bridge,
    sample: bool,
    model: &Arc<FrozenFit>,
    load_intraday: F,
) -> anyhow::Result<Arc<ResidualReport>>
where
    F: Fn(String, Timeframe) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<(Vec<Bar>, tr_data::vault::StitchInfo)>>,
{
    if sample {
        anyhow::bail!("Residual ticker: sample / offline mode makes no provider calls. Frozen models are never scored on sample prices.");
    }
    model.validate().map_err(|e| anyhow::anyhow!("model invalid: {e}"))?;
    let tf = model.model.tf;
    if tf != Timeframe::D1 && !tf.is_intraday() {
        anyhow::bail!("Residual tickers support daily or intraday bars.");
    }
    let mut names = vec![model.model.symbol.clone()];
    names.extend(model.model.factors.iter().map(|f| f.symbol.clone()));
    let loaded = load_rows(cache, fresh, history, bridge, names, tf, load_intraday).await;
    let observed_at = loaded.iter().filter_map(|(_, at)| *at).min();
    let mut rows: Vec<NamedBars> = loaded.into_iter().map(|(r, _)| r).collect();
    let target = rows.remove(0);
    if let Some(e) = &target.error {
        anyhow::bail!("{} unavailable: {e}", tr_data::security(&target.symbol).ticker);
    }
    let source = std::iter::once(&target)
        .chain(&rows)
        .filter(|r| r.error.is_none())
        .map(|r| format!("{}: {}", tr_data::security(&r.symbol).ticker, r.source))
        .collect::<Vec<_>>()
        .join(" · ");
    let daily = tf == Timeframe::D1;
    let closed = target
        .bars
        .last()
        .map(|b| super::studio_corr::closed_thru(tf, tr_data::security(&target.symbol).class, b.ts))
        .unwrap_or(0);
    let model = model.clone();
    let report = tokio::task::spawn_blocking(move || {
        let _job = jobs.as_ref().map(|j| crate::perf::JobTimer::new(j, "StudioResidScore"));
        let key = resid::score_key(&model, &target, &rows, &source, daily, closed);
        let params = key.params;
        RESID_REPORTS.get_or_compute(key, || {
            Ok::<_, String>(resid::score(&model, &target, &rows, daily, &source, closed, observed_at, params))
        })
    })
    .await?
    .map_err(anyhow::Error::msg)?;
    Ok(report)
}

/// Checks on an immutable report, single-flight per key. Runs inside `spawn_blocking`.
pub(crate) fn check_blocking(model: &FrozenFit, report: &ResidualReport) -> Result<Arc<ResidualChecks>, String> {
    let key = resid::checks_key(model, report);
    RESID_CHECKS.get_or_compute(key, || Ok::<_, String>(resid::checks(model, report)))
}

/// Freeze the displayed report and write the next immutable version. Runs inside `spawn_blocking`.
pub(crate) fn save_blocking(
    root: &std::path::Path,
    source: FitSource,
    name: Option<String>,
    monitor: Option<usize>,
) -> Result<crate::studio_models::SavedFit, String> {
    // Display-only wall clock; never part of the digest or any key.
    let saved_at = chrono::Utc::now().timestamp();
    let fit = match source {
        FitSource::Lab(r) => resid::freeze_lab(&r, name.as_deref(), monitor, saved_at)?,
        FitSource::Relate { report, tf } => resid::freeze_relate(&report, tf, name.as_deref(), monitor, saved_at)?,
    };
    crate::studio_models::save(root, fit)
}

// ---- pane ----------------------------------------------------------------------------------------

/// Ephemeral view state. Nothing here ever triggers a request.
#[derive(Default)]
pub struct ResidUi {
    /// Chart shows z (saved σ units) instead of e.
    pub show_z: bool,
    env: Option<(u64, usize, bool, Arc<Envelope>)>,
}

#[derive(Default)]
pub struct ResidPane {
    pub model_id: String,
    pub token: u64,
    pub model: Option<Arc<FrozenFit>>,
    pub report: Option<Arc<ResidualReport>>,
    /// The last accepted report while a rescore is in flight.
    pub previous: Option<Arc<ResidualReport>>,
    pub checks: Option<Arc<ResidualChecks>>,
    pub listing: Option<Vec<ModelListing>>,
    pub unavailable: Option<ModelUnavailable>,
    pub error: Option<String>,
    pub check_error: Option<String>,
    pub list_error: Option<String>,
    pub loading_model: bool,
    pub scoring: bool,
    pub checking: bool,
    pub listing_loading: bool,
    pub sample_text: Option<String>,
    pub ui: ResidUi,
    /// Set by "Refit in Lab"; the app opens the Lab on the next frame.
    pub want_refit: bool,
    /// Set by "Attribute move"; the app opens ATTR with this model and report on the next frame.
    pub want_attr: bool,
    fresh: bool,
}

impl ResidPane {
    pub fn new(model_id: impl Into<String>) -> Self {
        Self { model_id: model_id.into(), ..Default::default() }
    }
    /// Orphans every in-flight response with a fresh process-wide token.
    fn retoken(&mut self) {
        self.token = crate::worker::studio_token();
        self.loading_model = false;
        self.scoring = false;
        self.checking = false;
        self.listing_loading = false;
    }
    /// Opens another model: nothing of the previous one survives.
    pub fn open(&mut self, id: impl Into<String>) {
        self.model_id = id.into();
        self.model = None;
        self.report = None;
        self.previous = None;
        self.checks = None;
        self.unavailable = None;
        self.error = None;
        self.check_error = None;
        self.sample_text = None;
        self.retoken();
    }
    /// Fresh re-read of every live series, rescore and recheck. The old report stays visible as a
    /// PREVIOUS SNAPSHOT until the new one arrives; a refusal clears it.
    pub fn reload(&mut self) {
        self.fresh = true;
        self.previous = self.report.take().or(self.previous.take());
        self.checks = None;
        self.error = None;
        self.check_error = None;
        self.retoken();
    }
    /// Sample / offline mode: nothing is shown and nothing is requested.
    pub fn clear_for_sample(&mut self) {
        self.report = None;
        self.previous = None;
        self.checks = None;
        self.error = None;
        self.check_error = None;
        self.scoring = false;
        self.checking = false;
        self.sample_text = Some(
            "Sample / offline: no provider calls; frozen models are never scored on sample prices.".into(),
        );
    }
    /// The pipeline: list or load, then score, then checks. One request per phase.
    pub fn drive(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        if self.token == 0 {
            self.retoken();
        }
        if self.model_id.is_empty() {
            if self.listing.is_none() && !self.listing_loading && self.list_error.is_none() {
                self.listing_loading = true;
                worker.send(Request::ListStudioFits { pane, token: self.token });
            }
            return;
        }
        let Some(model) = self.model.clone() else {
            if self.unavailable.is_none() && !self.loading_model {
                self.loading_model = true;
                worker.send(Request::LoadStudioFit { pane, token: self.token, id: self.model_id.clone() });
            }
            return;
        };
        if self.report.is_none() && self.error.is_none() && !self.scoring {
            self.scoring = true;
            worker.send(Request::ScoreStudioResidual {
                pane,
                token: self.token,
                model,
                fresh: std::mem::take(&mut self.fresh),
            });
            return;
        }
        if let Some(r) = self.report.clone()
            && self.checks.is_none()
            && !self.checking
            && self.check_error.is_none()
            && r.unavailable.is_none()
        {
            self.checking = true;
            worker.send(Request::CheckStudioResidual { pane, token: self.token, model, report: r });
        }
    }
    pub fn accept_list(&mut self, token: u64, result: Result<Vec<ModelListing>, String>) {
        if token != self.token {
            return;
        }
        self.listing_loading = false;
        match result {
            Ok(v) => self.listing = Some(v),
            Err(e) => self.list_error = Some(e),
        }
    }
    /// Accepted only for the current token and the model id this pane asked for.
    pub fn accept_loaded(&mut self, token: u64, id: &str, result: Result<Arc<FrozenFit>, ModelUnavailable>) {
        if token != self.token || id != self.model_id {
            return;
        }
        self.loading_model = false;
        match result {
            Ok(m) => {
                self.unavailable = None;
                self.model = Some(m);
            }
            Err(u) => {
                self.model = None;
                self.unavailable = Some(u);
            }
        }
    }
    /// Accepted only for the current token and the digest of the model this pane holds.
    pub fn accept_scored(&mut self, token: u64, model_digest: u64, result: Result<Arc<ResidualReport>, String>) {
        if token != self.token || self.model.as_ref().is_none_or(|m| m.content_digest != model_digest) {
            return;
        }
        self.scoring = false;
        self.report = None;
        self.previous = None;
        self.checks = None;
        match result {
            Ok(r) if r.model_digest != model_digest => self.error = Some("score does not match the model".into()),
            Ok(r) => match r.provenance.require_verified("Residual ticker") {
                Ok(()) => {
                    self.error = None;
                    self.report = Some(r);
                }
                Err(e) => self.error = Some(e),
            },
            Err(e) => self.error = Some(e),
        }
    }
    /// Accepted only for the current token, the model digest and the score key of the displayed report.
    pub fn accept_checked(
        &mut self,
        token: u64,
        model_digest: u64,
        score_key: u64,
        result: Result<Arc<ResidualChecks>, String>,
    ) {
        if token != self.token
            || self.model.as_ref().is_none_or(|m| m.content_digest != model_digest)
            || self.report.as_ref().is_none_or(|r| r.key != score_key)
        {
            return;
        }
        self.checking = false;
        match result {
            Ok(c) if c.score_key == score_key && c.model_digest == model_digest => {
                self.check_error = None;
                self.checks = Some(c);
            }
            Ok(_) => self.check_error = Some("checks do not match the displayed score".into()),
            Err(e) => self.check_error = Some(e),
        }
    }
}

// ---- commands ------------------------------------------------------------------------------------

impl TerminalApp {
    /// `STUDIO resid [save|open|check]` and the `RESID …` aliases.
    pub(super) fn open_resid(&mut self, spec: &str) {
        let args = match ResidArgs::parse(spec) {
            Ok(a) => a,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let focus = self.focus;
        let focused_resid = self.panes.iter().any(|p| p.id == focus && matches!(p.kind, PaneKind::StudioResid(_)));
        match args {
            ResidArgs::List => {
                self.status = "Residual tickers · frozen fit scored on live data, never refit".into();
                self.open_panel(PaneKind::StudioResid(Box::new(ResidPane::new(String::new()))));
            }
            ResidArgs::Open(id) => {
                self.status = format!("Residual ticker {id} · scored, not refit");
                if focused_resid {
                    if let Some(PaneKind::StudioResid(p)) =
                        self.panes.iter_mut().find(|p| p.id == focus).map(|p| &mut p.kind)
                    {
                        p.open(id);
                    }
                } else {
                    self.open_panel(PaneKind::StudioResid(Box::new(ResidPane::new(id))));
                }
            }
            ResidArgs::Check(id) => {
                self.status = "Residual check · fresh reload, rescore and checks".into();
                match (id, focused_resid) {
                    (Some(id), _) => {
                        let mut p = ResidPane::new(id);
                        p.fresh = true;
                        self.open_panel(PaneKind::StudioResid(Box::new(p)));
                    }
                    (None, true) => {
                        if let Some(PaneKind::StudioResid(p)) =
                            self.panes.iter_mut().find(|p| p.id == focus).map(|p| &mut p.kind)
                        {
                            p.reload();
                        }
                    }
                    (None, false) => {
                        self.status = "STUDIO resid check needs a focused RESID pane or an id: STUDIO resid check <id>".into()
                    }
                }
            }
            ResidArgs::Save { name, monitor } => self.request_save(name, monitor),
        }
    }

    /// Freezes the focused Lab or Relate pane's displayed report.
    fn request_save(&mut self, name: Option<String>, monitor: Option<usize>) {
        let focus = self.focus;
        let Some(i) = self.idx(focus) else { return };
        let token = crate::worker::studio_token();
        let source = match &mut self.panes[i].kind {
            PaneKind::StudioLab(p) => match p.report.clone() {
                Some(r) if r.fit.is_some() => {
                    p.save = SaveState { pending: Some(token), msg: None };
                    Some(FitSource::Lab(r))
                }
                _ => None,
            },
            kind => match kind.corr_mut() {
                Some(c) => match (c.report.clone(), super::studio_corr::parse_spec(&c.spec)) {
                    (Some(r), Ok((_, tf))) if r.relate.as_ref().is_some_and(|x| x.fit.is_some()) => {
                        c.save = SaveState { pending: Some(token), msg: None };
                        Some(FitSource::Relate { report: r, tf })
                    }
                    _ => None,
                },
                None => None,
            },
        };
        match source {
            Some(source) => {
                self.status = "Saving residual model…".into();
                self.worker.send(Request::SaveStudioFit { pane: focus, token, name, monitor, source });
            }
            None => {
                self.status = "STUDIO resid save needs a focused Lab or Relate pane with a fit".into();
            }
        }
    }

    /// Routes `StudioFitSaved`: only the pane whose pending token matches reacts.
    pub(super) fn on_fit_saved(&mut self, pane: PaneId, token: u64, result: Result<crate::studio_models::SavedFit, String>) {
        let Some(i) = self.idx(pane) else { return };
        let save = match &mut self.panes[i].kind {
            PaneKind::StudioLab(p) => Some(&mut p.save),
            kind => kind.corr_mut().map(|c| &mut c.save),
        };
        let Some(save) = save else { return };
        if save.pending != Some(token) {
            return;
        }
        save.pending = None;
        match result {
            Ok(saved) => {
                save.msg = Some(format!("Saved {}", saved.id));
                self.status = format!("Saved {} · STUDIO resid open {}", saved.id, saved.id);
                self.open_panel(PaneKind::StudioResid(Box::new(ResidPane::new(saved.id))));
            }
            Err(e) => {
                save.msg = Some(e.clone());
                self.status = e;
            }
        }
    }

    pub(super) fn studio_resid_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("resid pane");
        let sample = self.provider.eq_ignore_ascii_case("sample");
        let PaneKind::StudioResid(pane) = &mut self.panes[i].kind else { return vec![] };
        let actions = show(ui, id, pane, &self.worker, sample);
        if std::mem::take(&mut pane.want_attr)
            && let Some(m) = pane.model.clone()
        {
            let r = pane.report.clone();
            self.status = format!("Attribution of {} · latest complete move, scored report reused, not refit", m.id);
            self.open_panel(PaneKind::StudioAttr(Box::new(super::studio_attribution::AttrPane::from_resid(
                m,
                r,
                Default::default(),
            ))));
            return actions;
        }
        let PaneKind::StudioResid(pane) = &mut self.panes[i].kind else { return actions };
        if std::mem::take(&mut pane.want_refit)
            && let Some(m) = pane.model.clone()
        {
            let next = m.id.rsplit_once("-v").and_then(|(_, n)| n.parse::<u32>().ok()).map_or(2, |n| n + 1);
            self.status = format!(
                "Refit in Lab · saving creates {}-v{next}; {} is unchanged",
                m.slug(),
                m.id
            );
            let tf = m.model.tf;
            self.open_panel(PaneKind::StudioLab(Box::new(super::studio_lab::LabPane::new(
                m.model.symbol.clone(),
                tf,
                m.lab_spec(),
            ))));
        }
        actions
    }
}

// ---- UI helpers ----------------------------------------------------------------------------------

pub(super) fn ui_text(s: impl Into<String>, size: f32, ink: Color32) -> RichText {
    RichText::new(s).font(theme::ui_font(size)).color(ink)
}
pub(super) fn mono_text(s: impl Into<String>, size: f32, ink: Color32) -> RichText {
    RichText::new(s).font(theme::mono(size)).color(ink)
}
/// Scientific notation with an explicit sign and a true minus.
fn sci(v: f64) -> String {
    format!("{v:+.3e}").replacen('-', "\u{2212}", 1)
}
fn fixed(v: f64, digits: usize) -> String {
    format!("{v:+.digits$}").replacen('-', "\u{2212}", 1)
}
pub(super) fn date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).map_or_else(|| "—".into(), |d| d.format("%Y-%m-%d").to_string())
}

fn clock(r: &ResidualReport, tf: Timeframe) -> String {
    match r.observed_at {
        Some(t) if tf == Timeframe::D1 => format!("{} (archive load time)", stamp(t)),
        Some(t) => stamp(t),
        None => "observation clock unavailable".into(),
    }
}

fn receipt(ui: &mut Ui, r: &ResidualReport, tf: Timeframe) {
    let parts = crate::widgets::chip_parts(&r.source_receipts);
    crate::widgets::source_chip(ui, r.provenance.label(), &parts, || {
        format!("CLOSED THRU {} · {} · {}", stamp(r.closed_thru), clock(r, tf), r.source)
    });
}

pub(super) fn caution_corner(ui: &Ui, rect: Rect) {
    ui.painter().line_segment([rect.right_top(), rect.right_top() + vec2(-5., 5.)], Stroke::new(1., theme::caution()));
}

fn ribbon(ui: &mut Ui, fit: &FrozenFit) {
    egui::Frame::new().fill(theme::card()).inner_margin(6.).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(mono_text("FROZEN FIT · scored, not refit", 11., theme::text()));
            ui.label(mono_text(&fit.id, 11., theme::text()));
        });
        ui.label(mono_text(
            format!(
                "saved {} · trained thru {} · origin {} · n_train {} · μ_saved {} · σ_saved {:.4e} · digest {:08x}",
                date(fit.saved_at),
                stamp(fit.trained_thru),
                match fit.model.origin {
                    resid::FitOrigin::Lab => "Lab",
                    resid::FitOrigin::Relate => "Relate",
                },
                fit.model.n_train,
                sci(fit.residual_mean),
                fit.residual_sigma,
                fit.content_digest >> 32
            ),
            10.,
            theme::muted(),
        ));
    });
}

pub(super) fn badges(ui: &mut Ui, r: &ResidualReport) {
    for (symbol, check) in &r.revision {
        let ticker = tr_data::security(symbol).ticker;
        let text = match check {
            InputCheck::Unchanged => continue,
            InputCheck::PartialCoverage => format!("PARTIAL REVISION COVERAGE · {ticker} · legacy training span"),
            InputCheck::Revised { .. } => format!("INPUT REVISED · {ticker}"),
            InputCheck::SourceChanged { saved, live } => format!("SOURCE CHANGED · {ticker} · was {saved} · now {live}"),
            InputCheck::NotCovered { live_first_ts } => format!(
                "TRAINING SPAN NOT RE-VERIFIABLE (live window starts {}) · {ticker}",
                stamp(*live_first_ts)
            ),
        };
        let resp = ui.label(mono_text(text, 10.5, theme::caution()));
        caution_corner(ui, resp.rect);
        ui.label(ui_text("the saved model is unchanged", 10., theme::muted()));
    }
}

// ---- MathCards -----------------------------------------------------------------------------------

fn base_card(fit: &FrozenFit, r: &ResidualReport, title: &str, formula: &str, n: usize, unit: &'static str) -> MathCard {
    let mut c = MathCard::new(title, formula)
        .n(n, unit)
        .span(stamp(fit.model.train_start), stamp(fit.trained_thru))
        .closed_thru(stamp(r.closed_thru))
        .source(&r.source)
        .provenance(r.provenance)
        .input("observation clock", clock(r, fit.model.tf))
        .input("model", format!("{} · digest {:08x}", fit.id, fit.content_digest >> 32));
    if let Some(why) = &r.unavailable {
        c = c.unavailable(why.clone());
    }
    c
}

fn term_name(fit: &FrozenFit, j: usize) -> String {
    if j == 0 { "intercept".into() } else { LabSpec::term_label(&fit.model.factors[j - 1]) }
}

fn score_card(fit: &FrozenFit, r: &ResidualReport, i: usize) -> MathCard {
    let p = r.point(i);
    let mut c = base_card(
        fit,
        r,
        "RESID SCORE · resid.score",
        "e = y − ŷ, ŷ = β₀ + Σ βⱼ·xⱼ (saved β; never refit) · z = (e − μ_saved)/σ_saved",
        fit.model.n_train,
        "training rows (saved)",
    )
    .span(stamp(fit.model.train_start), stamp(fit.trained_thru))
    .input("bar", stamp(p.ts))
    .input("segment", p.segment.label().to_string())
    .input("μ_saved / σ_saved", format!("{} / {:.4e}", sci(fit.residual_mean), fit.residual_sigma))
    .input("unit", r.residual.unit.label().to_string());
    match (r.y[i], r.fitted[i], p.residual, p.z) {
        (Some(y), Some(yh), Some(e), Some(z)) => {
            let mut terms = vec![format!("{}", sci(fit.model.coefficients[0]))];
            for j in 1..fit.model.k {
                let x = r.x.get(j - 1).and_then(|c| c.get(i)).copied().flatten();
                terms.push(format!("({})({})", sci(fit.model.coefficients[j]), x.map_or("—".into(), sci)));
            }
            c = c
                .input("y", sci(y))
                .input("ŷ = β·x", format!("{} = {}", terms.join(" + "), sci(yh)))
                .input("e = y − ŷ", sci(e))
                .input("z", fixed(z, 2))
                .value(format!("e {} · z {}", sci(e), fixed(z, 2)));
        }
        _ => c = c.unavailable("no residual at this bar: a saved input is missing here (nothing is imputed)"),
    }
    if p.segment != ResidSegment::Scored {
        c = c.caveat("TRAIN rows are in-sample by construction and EMBARGO rows overlap training support; checks use SCORED only");
    }
    if fit.model.target.is_level() || fit.model.factors.iter().any(|f| f.transform.is_level()) {
        c = c.caveat("levels may be nonstationary; a saved level model can drift away from its training relation");
    }
    c
}

fn z_card(fit: &FrozenFit, r: &ResidualReport) -> MathCard {
    let last = r.scored.last().map(|e| (e - fit.residual_mean) / fit.residual_sigma);
    let c = base_card(
        fit,
        r,
        "RESID Z · resid.z",
        "z = (e − μ_saved)/σ_saved, σ_saved = √(SSE/(n−k)) of the training fit; descriptive, not a probability",
        fit.model.n_train,
        "training rows (saved)",
    )
    .input("μ_saved", sci(fit.residual_mean))
    .input("σ_saved", format!("{:.6e}", fit.residual_sigma))
    .note("no p-value or star per point: z is a standardisation, not a test");
    match last {
        Some(z) => c.value(fixed(z, 2)),
        None => c.unavailable("no SCORED residual yet"),
    }
}

fn adf_card(fit: &FrozenFit, r: &ResidualReport, s: &CheckState<resid::AdfResult>) -> MathCard {
    let mut c = base_card(
        fit,
        r,
        "RESID ADF · resid.adf",
        "Δe_t = α + γ e_{t−1} + Σ δᵢ Δe_{t−i} + u_t; τ = γ̂/se(γ̂); H0 unit root; reject when τ < cv (Dickey–Fuller, MacKinnon N=1)",
        0,
        "scored bars",
    )
    .caveat("not an Engle–Granger cointegration certificate: training residuals are never tested; the frozen coefficients were estimated on a disjoint earlier sample");
    match s {
        CheckState::Ready(a) => {
            c = c
                .n(a.t_len, "scored bars")
                .span(stamp(a.window.0), stamp(a.window.1))
                .input("τ", fixed(a.tau, 3))
                .input("γ̂", sci(a.gamma))
                .input("lag p* (BIC, Schwert ≤ max)", format!("{} (≤ {})", a.lag, a.max_lag))
                .input("n_eff", a.n_eff.to_string())
                .input("cv 1% / 5% / 10%", format!("{} / {} / {}", fixed(a.crit[0], 3), fixed(a.crit[1], 3), fixed(a.crit[2], 3)))
                .note("declared family: RESID checks per model = {ADF, CUSUM}; the 5% verdict is the only one printed")
                .value(format!("τ {}", fixed(a.tau, 2)));
        }
        CheckState::Pending { have, need, .. } => c = c.n(*have, "scored bars").unavailable(resid::adf_pending_text(*have, *need)),
        CheckState::Unavailable(why) => c = c.unavailable(why.clone()),
    }
    c
}

fn monitor_card(fit: &FrozenFit, r: &ResidualReport, s: &CheckState<MonitorResult>) -> MathCard {
    let mut c = base_card(
        fit,
        r,
        "RESID MONITOR · resid.monitor",
        "Sₖ = Σ_{i≤k}(eᵢ − μ_saved), D_k = max_{j≤k}|S_j|; fixed horizon m; critical = 475th order statistic of D*_m over B=499 stationary-bootstrap draws of the saved training residuals",
        0,
        "scored residuals",
    )
    .caveat("assumes the residual process is stable and locally stationary; the bootstrap block is printed; the band is pointwise");
    match s {
        CheckState::Ready(m) => {
            c = c
                .n(m.k, "scored residuals")
                .input("m (fixed at save)", m.m.to_string())
                .input("D_k", sci(m.d_k))
                .input("critical (95%)", sci(m.critical))
                .input("B / block", format!("{} / {}", m.draws, m.block))
                .input("seed", format!("{:016x} (content digest)", fit.content_digest))
                .input("breaks crossed", m.breaks_crossed.to_string());
            c = match m.p {
                Some(p) => c.input("p = (1 + #{D* ≥ D})/(B+1)", format!("{p:.3}")).value(format!("p {p:.3}")),
                None => c.note(format!("partial {}/{} · descriptive; no verdict before k = m", m.k, m.m)),
            };
        }
        CheckState::Pending { have, need, why } => c = c.n(*have, "scored residuals").unavailable(format!("{why} ({have}/{need})")),
        CheckState::Unavailable(why) => c = c.unavailable(why.clone()),
    }
    c
}

fn half_life_card(fit: &FrozenFit, r: &ResidualReport, s: &CheckState<resid::HalfLife>) -> MathCard {
    let mut c = base_card(
        fit,
        r,
        "RESID HALF-LIFE · resid.halflife",
        "e_t = a + φ e_{t−1} + u; φ̃ = (nφ̂ + 1)/(n − 3); half-life = −ln 2 / ln φ̃ bars when 0 < φ̃ < 1",
        0,
        "scored pairs",
    )
    .caveat("descriptive; no interval is printed (a forced finite interval would hide non-reversion)");
    match s {
        CheckState::Ready(h) => {
            c = c.n(h.n, "scored pairs").input("φ̂ / φ̃", format!("{:.4} / {:.4}", h.phi_hat, h.phi));
            c = match (h.bars, &h.reason) {
                (Some(b), _) => c.value(format!("{b:.2} bars")),
                (None, Some(why)) => c.unavailable(why.clone()),
                _ => c,
            };
        }
        CheckState::Pending { have, need, .. } => c = c.unavailable(format!("Half-life pending: {have}/{need} scored bars")),
        CheckState::Unavailable(why) => c = c.unavailable(why.clone()),
    }
    c
}

// ---- charts --------------------------------------------------------------------------------------

/// Min/max per pixel column of the plotted series.
struct Envelope {
    w: usize,
    vals: Vec<(f64, f64)>,
    brk: Vec<bool>,
    /// First column that is not TRAIN, and first column that is SCORED.
    embargo_at: Option<usize>,
    scored_at: Option<usize>,
}

fn envelope(r: &ResidualReport, z: bool, w: usize) -> Envelope {
    let w = w.max(1);
    let s = if z { &r.z } else { &r.residual };
    let mut e = Envelope {
        w,
        vals: vec![(f64::INFINITY, f64::NEG_INFINITY); w],
        brk: vec![false; w],
        embargo_at: None,
        scored_at: None,
    };
    let n = s.ts.len().max(1);
    for i in 0..s.ts.len() {
        let c = (i * w / n).min(w - 1);
        if let Some(v) = s.values[i] {
            e.vals[c].0 = e.vals[c].0.min(v);
            e.vals[c].1 = e.vals[c].1.max(v);
            if s.break_before[i] {
                e.brk[c] = true;
            }
        }
        match r.segment[i] {
            ResidSegment::Embargo if e.embargo_at.is_none() => e.embargo_at = Some(c),
            ResidSegment::Scored if e.scored_at.is_none() => {
                e.scored_at = Some(c);
                e.embargo_at.get_or_insert(c);
            }
            _ => {}
        }
    }
    e
}

fn env_for(st: &mut ResidUi, r: &ResidualReport, w: usize) -> Arc<Envelope> {
    if let Some((k, ww, z, e)) = &st.env
        && *k == r.key
        && *ww == w
        && *z == st.show_z
    {
        return e.clone();
    }
    let e = Arc::new(envelope(r, st.show_z, w));
    st.env = Some((r.key, w, st.show_z, e.clone()));
    e
}

#[allow(clippy::too_many_arguments)]
fn env_line(
    p: &egui::Painter,
    rect: Rect,
    e: &Envelope,
    cols: std::ops::Range<usize>,
    lo: f64,
    hi: f64,
    stroke: Stroke,
) {
    let span = (hi - lo).max(1e-300);
    let x_of = |c: usize| rect.left() + rect.width() * (c as f32 + 0.5) / e.w as f32;
    let y_of = |v: f64| rect.bottom() - 2. - (rect.height() - 4.) * ((v - lo) / span) as f32;
    let mut line: Vec<Pos2> = Vec::new();
    let flush = |line: &mut Vec<Pos2>| {
        if line.len() > 1 {
            p.add(egui::Shape::line(std::mem::take(line), stroke));
        }
        line.clear();
    };
    for c in cols {
        let (a, b) = e.vals[c];
        if !a.is_finite() {
            flush(&mut line);
            continue;
        }
        if e.brk[c] {
            flush(&mut line);
        }
        let (first, second) = if c % 2 == 0 { (a, b) } else { (b, a) };
        line.push(pos2(x_of(c), y_of(first)));
        if (first - second).abs() > 0. {
            line.push(pos2(x_of(c), y_of(second)));
        }
    }
    flush(&mut line);
}

fn dashed_h(p: &egui::Painter, x0: f32, x1: f32, y: f32, stroke: Stroke) {
    let mut x = x0;
    while x < x1 {
        p.line_segment([pos2(x, y), pos2((x + 8.).min(x1), y)], stroke);
        x += 14.;
    }
}

fn residual_chart(ui: &mut Ui, fit: &FrozenFit, r: &ResidualReport, st: &mut ResidUi, width: f32) {
    ui.horizontal_wrapped(|ui| {
        ui.label(ui_text("RESIDUAL", 10., theme::muted()));
        for (z, label) in [(false, "e"), (true, "z")] {
            if ui.selectable_label(st.show_z == z, RichText::new(label).font(theme::mono(11.))).clicked() {
                st.show_z = z;
            }
        }
        ui.label(ui_text(
            if st.show_z { r.z.unit.label().to_string() } else { r.residual.unit.label().to_string() },
            10.,
            theme::muted(),
        ));
    });
    let w = width.max(80.) as usize;
    let e = env_for(st, r, w);
    // Fixed guides from the SAVED μ and σ (never from the live data).
    let (mu, sigma) = if st.show_z { (0., 1.) } else { (fit.residual_mean, fit.residual_sigma) };
    let (mut lo, mut hi) = (mu - 2. * sigma, mu + 2. * sigma);
    for (a, b) in e.vals.iter().filter(|(a, _)| a.is_finite()) {
        lo = lo.min(*a);
        hi = hi.max(*b);
    }
    let pad = (hi - lo) * 0.04;
    let (lo, hi) = (lo - pad, hi + pad);
    // Hover: the fixed chart height is reserved whether or not data exist.
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 220.), Sense::hover());
    #[cfg(test)]
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("resid-probe"), rect));
    let p = ui.painter().with_clip_rect(rect);
    p.rect_stroke(rect, 0., Stroke::new(1., theme::grid()), egui::StrokeKind::Inside);
    let y_of = |v: f64| rect.bottom() - 2. - (rect.height() - 4.) * ((v - lo) / (hi - lo).max(1e-300)) as f32;
    let x_col = |c: usize| rect.left() + rect.width() * c as f32 / e.w as f32;
    // EMBARGO shading.
    if let (Some(a), Some(b)) = (e.embargo_at, e.scored_at)
        && b > a
    {
        p.rect_filled(
            Rect::from_min_max(pos2(x_col(a), rect.top()), pos2(x_col(b), rect.bottom())),
            0.,
            theme::grid().gamma_multiply(0.30),
        );
    }
    for (k, label) in [(-2., "−2σ saved"), (-1., "−1σ"), (1., "+1σ"), (2., "+2σ saved")] {
        let y = y_of(mu + k * sigma);
        dashed_h(&p, rect.left(), rect.right(), y, Stroke::new(1., theme::grid()));
        p.text(pos2(rect.right() - 3., y - 1.), Align2::RIGHT_BOTTOM, label, theme::ui_font(9.), theme::muted());
    }
    p.hline(rect.x_range(), y_of(mu), Stroke::new(1., theme::muted()));
    let ink = theme::series(0);
    let (train_end, scored_start) = (e.embargo_at.unwrap_or(e.w), e.scored_at.unwrap_or(e.w));
    env_line(&p, rect, &e, 0..train_end, lo, hi, Stroke::new(1., ink.gamma_multiply(0.45)));
    if train_end < scored_start {
        env_line(&p, rect, &e, train_end..scored_start, lo, hi, Stroke::new(1., ink.gamma_multiply(0.7)));
    }
    env_line(&p, rect, &e, scored_start..e.w, lo, hi, Stroke::new(1.3, ink));
    if train_end > 0 {
        p.text(rect.left_top() + vec2(4., 2.), Align2::LEFT_TOP, "TRAIN · in-sample", theme::ui_font(9.), theme::muted());
    }
    if let Some(a) = e.embargo_at.filter(|a| *a > 0 || e.scored_at.is_some()) {
        let x = x_col(a);
        p.vline(x, rect.y_range(), Stroke::new(1., theme::muted()));
        p.text(pos2(x + 3., rect.top() + 12.), Align2::LEFT_TOP, "TRAIN | SCORED", theme::ui_font(9.), theme::muted());
    }
    let n = r.residual.ts.len();
    let pointer = resp.hover_pos();
    if let (Some(pos), true) = (pointer, n > 0) {
        p.vline(pos.x, rect.y_range(), Stroke::new(1., theme::muted().gamma_multiply(0.6)));
    }
    // An empty series has no point to describe (`r.point(0)` would index out of range).
    if n > 0 {
        resp.math_card(|| {
            let at = pointer.map_or(n - 1, |pos| {
                (((pos.x - rect.left()) / rect.width().max(1.)).clamp(0., 1.) * (n - 1) as f32).round() as usize
            });
            score_card(fit, r, at.min(n - 1))
        });
    }
}

fn monitor_chart(ui: &mut Ui, fit: &FrozenFit, r: &ResidualReport, m: &MonitorResult, width: f32) {
    ui.label(ui_text(
        format!(
            "CUSUM · fixed horizon m={} · {}/{} observed · B={} · block {}",
            m.m, m.k, m.m, m.draws, m.block
        ),
        10.,
        theme::muted(),
    ));
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 140.), Sense::hover());
    let p = ui.painter().with_clip_rect(rect);
    p.rect_stroke(rect, 0., Stroke::new(1., theme::grid()), egui::StrokeKind::Inside);
    let big = m
        .envelope
        .iter()
        .flat_map(|(a, b)| [a.abs(), b.abs()])
        .chain(m.path.iter().map(|v| v.abs()))
        .chain([m.critical])
        .fold(1e-300, f64::max)
        * 1.05;
    let y_of = |v: f64| rect.center().y - (rect.height() - 6.) * 0.5 * (v / big) as f32;
    let x_of = |j: usize| rect.left() + rect.width() * (j as f32 + 0.5) / m.m.max(1) as f32;
    // Pointwise envelope: neutral band, one quad per step.
    let mut mesh = egui::Mesh::default();
    let fill = theme::grid().gamma_multiply(0.35);
    for (j, (a, b)) in m.envelope.iter().enumerate() {
        let (x0, x1) = (x_of(j) - rect.width() / (2. * m.m as f32), x_of(j) + rect.width() / (2. * m.m as f32));
        let base = mesh.vertices.len() as u32;
        for pt in [pos2(x0, y_of(*b)), pos2(x1, y_of(*b)), pos2(x1, y_of(*a)), pos2(x0, y_of(*a))] {
            mesh.vertices.push(egui::epaint::Vertex { pos: pt, uv: egui::epaint::WHITE_UV, color: fill });
        }
        mesh.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    p.add(egui::Shape::mesh(mesh));
    p.hline(rect.x_range(), y_of(0.), Stroke::new(1., theme::muted()));
    for s in [-1., 1.] {
        dashed_h(&p, rect.left(), rect.right(), y_of(s * m.critical), Stroke::new(1., theme::muted()));
    }
    p.text(rect.right_top() + vec2(-3., 2.), Align2::RIGHT_TOP, "±critical (95% of max|S|) · band pointwise 2.5–97.5%", theme::ui_font(9.), theme::muted());
    let pts: Vec<Pos2> = m.path.iter().enumerate().map(|(j, v)| pos2(x_of(j), y_of(*v))).collect();
    if pts.len() > 1 {
        p.add(egui::Shape::line(pts, Stroke::new(1.4, theme::series(0))));
    }
    let pointer = resp.hover_pos();
    resp.math_card(|| {
        let j = pointer
            .map_or(m.k.saturating_sub(1), |pos| {
                (((pos.x - rect.left()) / rect.width().max(1.)).clamp(0., 1.) * (m.m - 1) as f32).round() as usize
            })
            .min(m.k.saturating_sub(1));
        let s = CheckState::Ready(m.clone());
        let mut c = monitor_card(fit, r, &s);
        if let Some(v) = m.path.get(j) {
            c = c.input(format!("S_{}", j + 1), sci(*v));
        }
        c
    });
}

// ---- tiles ---------------------------------------------------------------------------------------

fn tiles(ui: &mut Ui, fit: &FrozenFit, r: &ResidualReport, checks: Option<&ResidualChecks>, computing: bool) {
    let last_z = r.scored.last().map(|e| (e - fit.residual_mean) / fit.residual_sigma);
    ui.horizontal_wrapped(|ui| {
        stat_tile(
            ui,
            "latest z · saved σ (SCORED)",
            &last_z.map_or("—".into(), |z| fixed(z, 2)),
            last_z.map_or(theme::muted(), theme::sign),
            || z_card(fit, r),
        );
        let scored_note = format!("{} scored · {} missing", r.scored.len(), r.scored_missing);
        stat_tile(ui, "SCORED bars", &scored_note, theme::text(), || z_card(fit, r));
    });
    let Some(c) = checks else {
        // Reserved space: the tiles never collapse while the checks compute.
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 56.), Sense::hover());
        ui.painter().rect_stroke(rect, 0., Stroke::new(1., theme::grid()), egui::StrokeKind::Inside);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            if computing { "computing…" } else { "checks unavailable" },
            theme::ui_font(11.),
            theme::muted(),
        );
        return;
    };
    // ADF.
    let adf_line = match &c.adf {
        CheckState::Pending { have, need, .. } => resid::adf_pending_text(*have, *need),
        CheckState::Unavailable(why) => format!("ADF unavailable: {why}"),
        CheckState::Ready(a) => format!(
            "ADF τ = {} · H0 unit root · 5% cv {} (MacKinnon N=1, T={}) · lag {} (BIC ≤ {}) · {}",
            fixed(a.tau, 2).replacen('+', "", 1),
            fixed(a.crit[1], 2).replacen('+', "", 1),
            a.n_eff,
            a.lag,
            a.max_lag,
            if a.tau < a.crit[1] { "reject at 5%" } else { "do not reject at 5%" }
        ),
    };
    let monitor_line = match &c.monitor {
        CheckState::Pending { have, need, .. } => format!("Change check pending {have}/{need} · descriptive"),
        CheckState::Unavailable(why) => format!("Change check unavailable: {why}"),
        CheckState::Ready(m) if m.k < m.m => {
            let mut s = format!("Change check partial {}/{} · descriptive", m.k, m.m);
            if m.d_k >= m.critical {
                s += " · D is non-decreasing; the m-horizon test will reject.";
            }
            s
        }
        CheckState::Ready(m) => format!(
            "Change check m={} · D {} · critical {} · p {:.3} · {}",
            m.m,
            sci(m.d_k),
            sci(m.critical),
            m.p.unwrap_or(f64::NAN),
            if m.p.is_some_and(|p| p <= 0.05) { "reject at 5%" } else { "no evidence at 5%" }
        ),
    };
    let hl_line = match &c.half_life {
        CheckState::Pending { have, need, .. } => format!("Half-life pending: {have}/{need} scored bars"),
        CheckState::Unavailable(why) => format!("Half-life unavailable: {why}"),
        CheckState::Ready(h) => match (h.bars, &h.reason) {
            (Some(b), _) => format!("Half-life {b:.2} bars"),
            (None, Some(why)) => why.clone(),
            _ => "Half-life unavailable".into(),
        },
    };
    for (label, line, card) in [
        ("Stationarity check · ADF", adf_line, adf_card(fit, r, &c.adf)),
        ("Change check · CUSUM", monitor_line, monitor_card(fit, r, &c.monitor)),
        ("Half-life · AR(1)", hl_line, half_life_card(fit, r, &c.half_life)),
    ] {
        egui::Frame::new().fill(theme::card()).inner_margin(6.).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(ui_text(label, 10., theme::muted()));
            ui.label(mono_text(line, 11., theme::text())).on_hover_ui(|ui| card.show(ui));
        });
    }
}

fn coefficient_table(ui: &mut Ui, fit: &FrozenFit, r: &ResidualReport, width: f32) {
    let unit = r.residual.unit.label();
    let (hr, _) = ui.allocate_exact_size(vec2(width, 16.), Sense::hover());
    ui.painter().text(
        hr.left_center(),
        Align2::LEFT_CENTER,
        format!("Term (mono) · β frozen [{unit} per unit of x]"),
        theme::ui_font(9.),
        theme::muted(),
    );
    for j in 0..fit.model.k {
        let (rect, resp) = ui.allocate_exact_size(vec2(width, 20.), Sense::hover());
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(rect, 0., theme::grid().gamma_multiply(0.2));
            p.rect_filled(Rect::from_min_size(rect.min, vec2(2., rect.height())), 0., theme::accent());
        }
        let b = fit.model.coefficients[j];
        p.text(rect.left_center() + vec2(6., 0.), Align2::LEFT_CENTER, term_name(fit, j), theme::mono(10.), theme::text());
        p.text(
            pos2(rect.right() - 4., rect.center().y),
            Align2::RIGHT_CENTER,
            if j == 0 { sci(b) } else { signed(b, 6) },
            theme::mono(10.),
            theme::sign(b),
        );
    }
}

// ---- panels --------------------------------------------------------------------------------------

/// The report body, painted from immutable `Arc`s. `computing` reserves the check tiles.
pub(crate) fn panel(
    ui: &mut Ui,
    id: PaneId,
    fit: &FrozenFit,
    r: &ResidualReport,
    checks: Option<&ResidualChecks>,
    computing: bool,
    st: &mut ResidUi,
) {
    ui.push_id(("resid-panel", id), |ui| {
        let width = ui.available_width();
        ribbon(ui, fit);
        if let Some(why) = &r.unavailable {
            let (rect, _) = ui.allocate_exact_size(vec2(width, 120.), Sense::hover());
            ui.painter().rect_filled(rect, 3., theme::card());
            ui.painter().text(rect.left_top() + vec2(8., 8.), Align2::LEFT_TOP, "RESIDUAL UNAVAILABLE", theme::mono(11.), theme::amber());
            let g = ui.painter().layout(why.clone(), theme::ui_font(11.), theme::text(), width - 16.);
            ui.painter().galley(rect.left_top() + vec2(8., 26.), g, theme::text());
            for (s, e) in &r.factor_errors {
                ui.label(ui_text(format!("{} · {e}", tr_data::security(s).ticker), 10., theme::caution()));
            }
            return;
        }
        badges(ui, r);
        receipt(ui, r, fit.model.tf);
        residual_chart(ui, fit, r, st, width);
        if let Some(CheckState::Ready(m)) = checks.map(|c| &c.monitor) {
            monitor_chart(ui, fit, r, m, width);
        }
        tiles(ui, fit, r, checks, computing);
        studio_heading(ui, "SAVED COEFFICIENTS");
        coefficient_table(ui, fit, r, width.min(520.));
        if r.scored_missing > 0 || !r.dropped.is_empty() {
            for (reason, n) in &r.dropped {
                ui.label(mono_text(format!("dropped {n} · {reason}"), 9.5, theme::muted()));
            }
        }
        ui.label(ui_text(
            "residual = actual − saved fit, in the target's own unit · never refit · overnight bars sit inside the ADF segment for intraday cash series",
            9.5,
            theme::muted(),
        ));
    });
}

/// Recoverable card for a missing, corrupt or unreadable model. No numbers are painted.
pub(crate) fn model_unavailable(ui: &mut Ui, u: &ModelUnavailable) {
    egui::Frame::new().fill(theme::card()).inner_margin(8.).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(mono_text(
            format!("Model `{}` unavailable: {} — {}", u.id, u.kind.label(), u.message),
            11.,
            theme::amber(),
        ));
        ui.label(ui_text("Open another saved model below, or save one from a Lab or Relate pane.", 10., theme::muted()));
    });
}

fn list_view(ui: &mut Ui, pane: &mut ResidPane) -> Option<String> {
    let mut open = None;
    if let Some(e) = &pane.list_error {
        ui.label(ui_text(format!("Could not list saved models: {e}"), 11., theme::down()));
    }
    match &pane.listing {
        None if pane.listing_loading => {
            ui.label(ui_text("Loading saved models…", 11., theme::muted()));
        }
        Some(l) if l.is_empty() => {
            ui.label(ui_text("No saved residual models yet. Open Regression Lab (STUDIO lab), fit a model, then Save residual · STUDIO resid open <id>", 11., theme::muted()));
        }
        Some(l) => {
            ui.label(ui_text("SAVED MODELS", 10., theme::muted()));
            for m in l {
                let label = match &m.problem {
                    None => format!(
                        "{} · {} · {} · trained thru {} · saved {}",
                        m.id,
                        tr_data::security(&m.symbol).ticker,
                        m.tf.label(),
                        stamp(m.trained_thru),
                        date(m.saved_at)
                    ),
                    Some(p) => format!("{} · {p}", m.id),
                };
                let ink = if m.problem.is_some() { theme::caution() } else { theme::text() };
                if ui.selectable_label(false, mono_text(label, 10.5, ink)).clicked() && m.problem.is_none() {
                    open = Some(m.id.clone());
                }
            }
        }
        None => {
            ui.label(ui_text("Open a saved model: STUDIO resid open <id>", 11., theme::muted()));
        }
    }
    open
}

pub(super) fn show(ui: &mut Ui, id: PaneId, pane: &mut ResidPane, worker: &crate::worker::Worker, sample: bool) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut reload = false;
    ui.horizontal_wrapped(|ui| {
        if pane.listing.as_ref().is_some_and(|l| l.is_empty()) && ui.small_button("Open Regression Lab").clicked() {
            actions.push(Action::Command("STUDIO lab".into()));
        }
        studio_title(ui, "RESIDUAL");
        if let Some(m) = &pane.model {
            ui.label(mono_text(
                format!("{} · {} · {}", tr_data::security(&m.model.symbol).ticker, m.model.tf.label(), m.id),
                11.,
                theme::text(),
            ));
        } else if !pane.model_id.is_empty() {
            ui.label(mono_text(&pane.model_id, 11., theme::text()));
        }
        reload |= ui.small_button("Reload").on_hover_text("Re-read every series, rescore and recheck").clicked();
        reload |= ui.small_button("Check now").on_hover_text("Fresh reload, rescore and checks").clicked();
        if let Some(m) = &pane.model {
            if ui.small_button("Copy command").on_hover_text("Copy a command that reopens this model").clicked() {
                ui.ctx().copy_text(format!("STUDIO resid open {}", m.id));
            }
            if ui
                .small_button("Refit in Lab (new version)")
                .on_hover_text("Opens a Lab with this model's inputs; saving creates the next version and leaves this one unchanged")
                .clicked()
            {
                pane.want_refit = true;
            }
            if ui
                .small_button("Attribute move")
                .on_hover_text("Opens ATTR with this model and its scored report: actual = drift + factors + residual")
                .clicked()
            {
                pane.want_attr = true;
            }
        }
    });
    if sample {
        pane.clear_for_sample();
    } else {
        pane.sample_text = None;
        if reload && pane.model.is_some() {
            pane.reload();
        }
        pane.drive(id, worker);
    }
    let mut open: Option<String> = None;
    egui::ScrollArea::vertical().id_salt(("resid-scroll", id)).auto_shrink([false, false]).show(ui, |ui| {
        if let Some(t) = pane.sample_text.clone() {
            ui.label(ui_text(t, 12., theme::caution()));
            if !pane.model_id.is_empty() {
                ui.label(mono_text(format!("model {}", pane.model_id), 11., theme::muted()));
            }
            return;
        }
        if let Some(u) = pane.unavailable.clone() {
            model_unavailable(ui, &u);
            open = list_view(ui, pane);
            return;
        }
        if pane.model_id.is_empty() {
            ui.label(ui_text("RESIDUAL TICKERS · a frozen Lab / Relate fit scored on live data, never refit", 11., theme::text()));
            open = list_view(ui, pane);
            return;
        }
        let Some(model) = pane.model.clone() else {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 420.), Sense::hover());
            ui.painter().rect_stroke(rect, 0., Stroke::new(1., theme::grid()), egui::StrokeKind::Inside);
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Loading model…", theme::ui_font(12.), theme::muted());
            return;
        };
        if let Some(r) = pane.report.clone() {
            let (checks, computing) = (pane.checks.clone(), pane.checking);
            panel(ui, id, &model, &r, checks.as_deref(), computing, &mut pane.ui);
            if let Some(e) = &pane.check_error {
                ui.label(ui_text(format!("checks failed: {e}"), 10., theme::down()));
            }
        } else if let (true, Some(prev)) = (pane.scoring, pane.previous.clone()) {
            ui.label(ui_text("PREVIOUS SNAPSHOT · rescoring…", 11., theme::muted()));
            panel(ui, id, &model, &prev, None, true, &mut pane.ui);
        } else if pane.scoring {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 420.), Sense::hover());
            ui.painter().rect_stroke(rect, 0., Stroke::new(1., theme::grid()), egui::StrokeKind::Inside);
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                format!("Scoring {} series…", 1 + model.model.factors.len()),
                theme::ui_font(12.),
                theme::muted(),
            );
        } else if let Some(e) = &pane.error {
            ribbon(ui, &model);
            egui::Frame::new().fill(theme::card()).inner_margin(8.).show(ui, |ui| {
                ui.label(ui_text(e, 12., theme::down()));
            });
        }
    });
    if let Some(id) = open {
        pane.open(id);
    }
    actions
}

// ---- fixtures (tests and the paint gate only) ------------------------------------------------------

/// Dev/test fixture: a frozen 7-factor model scored on its own live rows, with its checks.
#[cfg(test)]
pub(super) fn fixture(n: usize) -> (Arc<FrozenFit>, Arc<ResidualReport>, Arc<ResidualChecks>) {
    let (fit, target, rows) = tr_perf::fixtures::resid_model(n);
    let closed = target.bars.last().unwrap().ts + 86_400;
    let r = resid::score(&fit, &target, &rows, true, "benchmark fixture only", closed, None, 1);
    let c = resid::checks(&fit, &r);
    (Arc::new(fit), Arc::new(r), Arc::new(c))
}

#[cfg(test)]
mod tests;
