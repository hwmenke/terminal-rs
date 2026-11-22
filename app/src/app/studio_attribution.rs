//! Factor attribution (Studio slice 7): the ATTR pane. The move over a window, for a saved frozen
//! model, is split into `actual = drift + Σ factor contributions + residual`. The maths is a pure
//! function of the model and its scored report (`tr_data::studio_attribution`); this module only
//! drives the pipeline (list → load → score → attribute), keys nothing with a clock, and paints
//! immutable `Arc`s. It never refits, never loads data and makes no causal claim.
use super::studio_dist::{stamp, studio_heading, studio_title};
use super::studio_residual::{badges, caution_corner, date, mono_text, ui_text};
use super::{PaneKind, TerminalApp};
use crate::app::studio_corr::relate_ui::signed;
use crate::studio_models::{ModelListing, ModelUnavailable};
use crate::widgets::{MathCard, MathCardExt, stat_tile};
use crate::{congress_view::Action, dock::PaneId, theme, worker::Request};
use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use std::sync::{Arc, LazyLock};
use tr_core::Timeframe;
use tr_data::studio::Transform;
use tr_data::studio_attribution::{
    self as attr, AttrArgs, AttrMode, AttrWindow, AttributionReport, AttributionSe, ExposureKind, WindowLabel,
};
use tr_data::studio_residual::{FrozenFit, ResidualReport};

/// Attributions are small (≤ 5,000 path rows); keep the 8 most recent.
static ATTR_REPORTS: LazyLock<tr_data::report_cache::ReportCache> =
    LazyLock::new(|| tr_data::report_cache::ReportCache::new(8));

/// Pure attribution on an immutable scored report, single-flight per key. Runs inside `spawn_blocking`.
pub(crate) fn attribute_blocking(
    m: &FrozenFit,
    r: &ResidualReport,
    w: &AttrWindow,
) -> Result<Arc<AttributionReport>, String> {
    let key = attr::attribution_key(m, r, w);
    ATTR_REPORTS.get_or_compute(key, || Ok::<_, String>(attr::attribute(m, r, w)))
}

// ---- pane ----------------------------------------------------------------------------------------

/// Ephemeral view state. Nothing here ever triggers a request.
#[derive(Default)]
pub struct AttrUi {
    /// Which uncertainty the tile and table show (both are computed).
    pub se: AttributionSe,
    /// Stored covariance entry chosen by the estimator chips (`None` = the default entry).
    pub cov: Option<usize>,
    hover: Option<usize>,
}

#[derive(Default)]
pub struct AttrPane {
    pub model_id: String,
    /// Load / score token.
    pub token: u64,
    /// Attribution token; a window change never orphans an in-flight score.
    pub attr_token: u64,
    pub model: Option<Arc<FrozenFit>>,
    pub report: Option<Arc<ResidualReport>>,
    pub attribution: Option<Arc<AttributionReport>>,
    /// The last accepted attribution while a new window or rescore is in flight.
    pub previous: Option<Arc<AttributionReport>>,
    pub window: AttrWindow,
    pub listing: Option<Vec<ModelListing>>,
    pub unavailable: Option<ModelUnavailable>,
    pub error: Option<String>,
    pub attr_error: Option<String>,
    pub list_error: Option<String>,
    pub loading_model: bool,
    pub scoring: bool,
    pub attributing: bool,
    pub listing_loading: bool,
    pub sample_text: Option<String>,
    pub ui: AttrUi,
    fresh: bool,
}

impl AttrPane {
    pub fn new(model_id: impl Into<String>) -> Self {
        Self { model_id: model_id.into(), ..Default::default() }
    }
    /// Restores from layout words: an invalid id or window becomes empty / default.
    pub fn restore(model_id: &str, window: &str) -> Self {
        let id = if tr_data::studio_residual::valid_id(model_id) { model_id.to_string() } else { String::new() };
        let mut p = Self::new(id);
        if let Some(w) = AttrArgs::parse(window).ok().and_then(|a| a.window) {
            p.window = w;
        }
        p
    }
    /// Opened from a RESID pane: the model and (when it matches) the scored report are adopted
    /// without rescoring.
    pub fn from_resid(model: Arc<FrozenFit>, report: Option<Arc<ResidualReport>>, window: AttrWindow) -> Self {
        let mut p = Self::new(model.id.clone());
        p.window = window;
        p.report = report.filter(|r| r.model_digest == model.content_digest && r.unavailable.is_none());
        p.model = Some(model);
        p.retoken();
        p
    }
    pub fn is_daily(&self) -> bool {
        self.model.as_ref().is_some_and(|m| m.model.tf == Timeframe::D1)
    }
    /// Orphans every in-flight response with fresh process-wide tokens.
    fn retoken(&mut self) {
        self.token = crate::worker::studio_token();
        self.attr_token = crate::worker::studio_token();
        self.loading_model = false;
        self.scoring = false;
        self.attributing = false;
        self.listing_loading = false;
    }
    /// Another model: nothing of the previous one survives.
    pub fn open(&mut self, id: impl Into<String>) {
        self.model_id = id.into();
        self.model = None;
        self.report = None;
        self.attribution = None;
        self.previous = None;
        self.unavailable = None;
        self.error = None;
        self.attr_error = None;
        self.sample_text = None;
        self.retoken();
    }
    /// A new window on the same score: one new attribution, no rescore.
    pub fn set_window(&mut self, w: AttrWindow) {
        if w == self.window && (self.attribution.is_some() || self.attributing) {
            return;
        }
        self.window = w;
        self.previous = self.attribution.take().or(self.previous.take());
        self.attr_error = None;
        self.attr_token = crate::worker::studio_token();
        self.attributing = false;
    }
    /// Fresh re-read, rescore and reattribute. The old attribution stays as a PREVIOUS SNAPSHOT until
    /// the new one arrives; a refusal clears it.
    pub fn reload(&mut self) {
        self.fresh = true;
        self.previous = self.attribution.take().or(self.previous.take());
        self.report = None;
        self.error = None;
        self.attr_error = None;
        if self.model.is_none() {
            // No model yet: let the model list be fetched again.
            self.listing = None;
            self.list_error = None;
            self.listing_loading = false;
        }
        self.retoken();
    }
    /// Sample / offline mode: nothing is shown and nothing is requested.
    pub fn clear_for_sample(&mut self) {
        self.report = None;
        self.attribution = None;
        self.previous = None;
        self.error = None;
        self.attr_error = None;
        self.scoring = false;
        self.attributing = false;
        self.sample_text =
            Some("Sample / offline: no provider calls; frozen models are never attributed on sample prices.".into());
    }
    /// The pipeline, one phase at a time. Pure apart from the in-flight flags.
    pub fn next_request(&mut self, pane: PaneId) -> Option<Request> {
        if self.sample_text.is_some() {
            return None;
        }
        if self.token == 0 {
            self.retoken();
        }
        if self.model_id.is_empty() {
            if self.listing.is_none() && !self.listing_loading && self.list_error.is_none() {
                self.listing_loading = true;
                return Some(Request::ListStudioFits { pane, token: self.token });
            }
            return None;
        }
        let Some(model) = self.model.clone() else {
            if self.unavailable.is_none() && !self.loading_model {
                self.loading_model = true;
                return Some(Request::LoadStudioFit { pane, token: self.token, id: self.model_id.clone() });
            }
            return None;
        };
        let Some(report) = self.report.clone() else {
            if self.error.is_none() && !self.scoring {
                self.scoring = true;
                return Some(Request::ScoreStudioResidual {
                    pane,
                    token: self.token,
                    model,
                    fresh: std::mem::take(&mut self.fresh),
                });
            }
            return None;
        };
        if self.attribution.is_none() && !self.attributing && self.attr_error.is_none() && report.unavailable.is_none() {
            self.attributing = true;
            return Some(Request::StudioAttribution {
                pane,
                token: self.attr_token,
                model,
                report,
                window: self.window.clone(),
            });
        }
        None
    }
    pub fn drive(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        if let Some(r) = self.next_request(pane) {
            worker.send(r);
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
        self.attribution = None;
        let refuse = |s: &mut Self, e: String| {
            s.previous = None;
            s.error = Some(e);
        };
        match result {
            Ok(r) if r.model_digest != model_digest => refuse(self, "score does not match the model".into()),
            Ok(r) => match r.provenance.require_verified("Attribution") {
                Ok(()) => {
                    self.error = None;
                    if r.unavailable.is_some() {
                        // An unavailable score must not hide behind the old numbers.
                        self.previous = None;
                    }
                    self.report = Some(r);
                }
                Err(e) => refuse(self, e),
            },
            Err(e) => refuse(self, e),
        }
    }
    /// Accepted only for the current attr token, model digest, score key and window.
    pub fn accept_attributed(
        &mut self,
        token: u64,
        model_digest: u64,
        score_key: u64,
        window: &AttrWindow,
        result: Result<Arc<AttributionReport>, String>,
    ) {
        if token != self.attr_token
            || self.model.as_ref().is_none_or(|m| m.content_digest != model_digest)
            || self.report.as_ref().is_none_or(|r| r.key != score_key)
            || *window != self.window
        {
            return;
        }
        self.attributing = false;
        match result {
            Ok(a) if a.model_digest == model_digest && a.score_key == score_key && a.window == *window => {
                self.attr_error = None;
                self.previous = None;
                self.attribution = Some(a);
            }
            Ok(_) => {
                self.previous = None;
                self.attr_error = Some("attribution does not match the displayed score".into());
            }
            Err(e) => {
                self.previous = None;
                self.attr_error = Some(e);
            }
        }
    }
}

// ---- commands ------------------------------------------------------------------------------------

impl TerminalApp {
    /// `STUDIO attr [model=<id>] [obs=N | from=<t>] [to=<t>]` and the `ATTR …` aliases.
    pub(super) fn open_attr(&mut self, spec: &str) {
        let args = match AttrArgs::parse(spec) {
            Ok(a) => a,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let focus = self.focus;
        let resid = self.panes.iter().find(|p| p.id == focus).and_then(|p| match &p.kind {
            PaneKind::StudioResid(r) => r.model.clone().map(|m| (m, r.report.clone())),
            _ => None,
        });
        let focused_attr = self.panes.iter().any(|p| p.id == focus && matches!(p.kind, PaneKind::StudioAttr(_)));
        let window = args.window.clone().unwrap_or_default();
        self.status = "Attribution · actual = drift + factors + residual, frozen fit never refit".into();
        match (args.model, args.window) {
            // A window on the focused ATTR pane: one new attribution, no rescore.
            (None, Some(w)) if focused_attr => {
                if let Some(PaneKind::StudioAttr(p)) = self.panes.iter_mut().find(|p| p.id == focus).map(|p| &mut p.kind) {
                    p.set_window(w);
                }
            }
            (None, w) => match resid {
                Some((m, r)) => self.open_panel(PaneKind::StudioAttr(Box::new(AttrPane::from_resid(m, r, w.unwrap_or_default())))),
                None if w.is_some() => {
                    self.status = "STUDIO attr needs model=<id> (or a focused RESID / ATTR pane) for a window".into()
                }
                None => self.open_panel(PaneKind::StudioAttr(Box::new(AttrPane::new(String::new())))),
            },
            (Some(id), _) => match resid {
                Some((m, r)) if m.id == id => {
                    self.open_panel(PaneKind::StudioAttr(Box::new(AttrPane::from_resid(m, r, window))))
                }
                _ => {
                    let mut p = AttrPane::new(id);
                    p.window = window;
                    self.open_panel(PaneKind::StudioAttr(Box::new(p)));
                }
            },
        }
    }

    pub(super) fn studio_attr_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("attr pane");
        let sample = self.provider.eq_ignore_ascii_case("sample");
        let PaneKind::StudioAttr(pane) = &mut self.panes[i].kind else { return vec![] };
        show(ui, id, pane, &self.worker, sample)
    }
}

// ---- formatting ----------------------------------------------------------------------------------

fn scale_of(t: Transform) -> (f64, &'static str) {
    match t {
        Transform::LogReturn { .. } | Transform::LogLevel => (1e4, "log bp"),
        Transform::Change { .. } => (1., "Δ price"),
        Transform::Level => (1., "price"),
    }
}

/// Fixed precision for one report: log bp 1 decimal; price 2 decimals if any |value| ≥ 1, else 4.
fn precision(a: &AttributionReport) -> usize {
    if a.display_unit == "log bp" {
        return 1;
    }
    let mx = [a.actual, a.predicted, a.intercept, a.residual]
        .into_iter()
        .chain(a.factors.iter().map(|f| f.contribution))
        .fold(0f64, |m, v| m.max(v.abs()));
    if mx * a.display_scale >= 1. { 2 } else { 4 }
}

fn beta_text(b: f64) -> String {
    if b.abs() < 1e-3 && b != 0. { format!("{b:+.3e}").replacen('-', "\u{2212}", 1) } else { signed(b, 4) }
}

fn unsigned(v: f64, digits: usize) -> String {
    format!("{v:.digits$}")
}

fn clock(a: &AttributionReport, daily: bool) -> String {
    match a.observed_at {
        Some(t) if daily => format!("{} (archive load time)", stamp(t)),
        Some(t) => stamp(t),
        None => "observation clock unavailable".into(),
    }
}

fn label_text(l: &WindowLabel) -> String {
    match l {
        WindowLabel::InSample => "IN-SAMPLE".into(),
        WindowLabel::OutOfSample => "OUT-OF-SAMPLE".into(),
        WindowLabel::Mixed { train, embargo, scored } => format!("MIXED {train}/{embargo}/{scored}"),
    }
}

fn mode_text(a: &AttributionReport) -> String {
    match a.mode {
        AttrMode::Sum { h } => format!("m={} obs (h={h})", a.m),
        AttrMode::Endpoint => "endpoint Δ".into(),
    }
}

// ---- MathCards -----------------------------------------------------------------------------------

fn base_card(fit: &FrozenFit, a: &AttributionReport, title: &str, formula: &str) -> MathCard {
    let daily = fit.model.tf == Timeframe::D1;
    let n_unit = match a.mode {
        AttrMode::Sum { .. } => "target observations in the window",
        AttrMode::Endpoint => "endpoint rows (a, b)",
    };
    let mut c = MathCard::new(title, formula)
        .n(a.m.max(1), n_unit)
        .span(stamp(a.from_ts), stamp(a.to_ts))
        .closed_thru(stamp(a.closed_thru))
        .source(&a.source)
        .provenance(a.provenance)
        .input("observation clock", clock(a, daily))
        .input("model", format!("{} · digest {:08x}", a.model_id, a.model_digest >> 32))
        .input("n_train", fit.model.n_train.to_string())
        .input("unit", a.display_unit.to_string());
    if let Some(why) = &a.unavailable {
        c = c.unavailable(why.clone());
    }
    c
}

fn window_card(fit: &FrozenFit, a: &AttributionReport) -> MathCard {
    base_card(fit, a, "ATTR WINDOW · attr.window", "window resolved on the scored grid, \"on or before\"")
        .input("anchor a", stamp(a.from_ts))
        .input("end b", stamp(a.to_ts))
        .input("m", a.m.to_string())
        .input("mode", mode_text(a))
        .input("label", label_text(&a.label))
        .input("trained thru", stamp(a.trained_thru))
        .input("embargo", format!("{} bars", a.embargo_bars))
}

fn actual_card(fit: &FrozenFit, a: &AttributionReport) -> MathCard {
    let (formula, tele) = match a.mode {
        AttrMode::Sum { h } => (
            format!("A = Σ_(t∈R) y_t over a stride-{h} chain of rows"),
            "telescopes exactly to the target's move over (a, b]",
        ),
        AttrMode::Endpoint => ("A = y_b − y_a".to_string(), "difference of the two endpoint rows; no sum over intermediate rows"),
    };
    let mut c = base_card(fit, a, "ATTR ACTUAL · attr.actual", &formula)
        .input("A", format!("{} {}", signed(a.actual * a.display_scale, precision(a)), a.display_unit))
        .note(tele);
    if let Some(s) = a.simple_actual {
        c = c.input("simple (expm1)", format!("{} bp · not additively decomposed", signed(s * 1e4, 1)));
    }
    c.value(signed(a.actual * a.display_scale, precision(a)))
}

fn drift_card(fit: &FrozenFit, a: &AttributionReport) -> MathCard {
    let b0 = fit.model.coefficients[0];
    let mut c = base_card(fit, a, "ATTR DRIFT · attr.drift", "c₀ = β₀·m (intercept × observations)")
        .input("β₀", beta_text(b0))
        .input("m", a.m.to_string());
    match a.mode {
        AttrMode::Sum { .. } => {
            c = c.input("c₀", format!("{} {}", signed(a.intercept * a.display_scale, precision(a)), a.display_unit));
            if let Some(se) = a.cov(None).and_then(|c| c.intercept_se) {
                c = c.input("SE |d₀|√V₀₀", unsigned(se * a.display_scale, precision(a)));
            }
        }
        AttrMode::Endpoint => c = c.note("no drift in an endpoint Δ: d₀ = 0"),
    }
    c
}

fn contribution_card(fit: &FrozenFit, a: &AttributionReport, j: usize, ci: Option<usize>) -> MathCard {
    let f = &a.factors[j];
    let (fs, funit) = scale_of(f.transform);
    let p = precision(a);
    let mut c = base_card(fit, a, "ATTR CONTRIBUTION · attr.contribution", "c_j = β_j·d_j (frozen exposure × observed factor move)")
        .input("factor", f.label.clone())
        .input("β_j", beta_text(f.beta))
        .input("d_j", format!("{} {funit}", signed(f.exposure * fs, if fs == 1. { 4 } else { 1 })))
        .input("lag", f.lag.to_string())
        .input("c_j", format!("{} {}", signed(f.contribution * a.display_scale, p), a.display_unit));
    c = match &f.kind {
        ExposureKind::Move { from_ts, to_ts } => c.input("move", format!("{} → {}", stamp(*from_ts), stamp(*to_ts))),
        ExposureKind::EndpointMove => c.input("move", "endpoint difference x_b − x_a"),
        ExposureKind::NotAMove { why } => c.input("move", format!("N/A: {why}")),
    };
    if let Some(cv) = ci.and_then(|i| a.covariances.get(i)) {
        c = c.input("estimator", cv.label.clone());
        match f.coefficient_se.get(ci.unwrap_or(0)).copied().flatten() {
            Some(se) => c = c.input("SE_j = |d_j|√V_jj", unsigned(se * a.display_scale, p)),
            None => c = c.input("SE_j", "unavailable: coefficient covariance not stored"),
        }
    }
    c.note("terms' SEs are not additive: the total uses dᵀVd (covariances included)")
}

fn residual_card(fit: &FrozenFit, a: &AttributionReport) -> MathCard {
    base_card(fit, a, "ATTR RESIDUAL · attr.residual", "R = A − Â (actual − predicted, exact)")
        .input("R", format!("{} {}", signed(a.residual * a.display_scale, precision(a)), a.display_unit))
        .input("Σe cross-check", signed(a.residual_scored_sum * a.display_scale, precision(a)))
        .value(signed(a.residual * a.display_scale, precision(a)))
}

fn se_card(fit: &FrozenFit, a: &AttributionReport, ci: Option<usize>, predictive: bool) -> MathCard {
    let p = precision(a);
    let rv = &a.residual_var;
    let mut c = if predictive {
        base_card(
            fit,
            a,
            "ATTR PREDICTIVE SE · attr.pred_se",
            "SE = √(dᵀVd + Var_W): coefficient estimation error plus the window's own residual variance",
        )
        .input("σ²_saved", format!("{:.6e}", rv.sigma2))
        .input("L (Bartlett)", rv.truncation.to_string())
        .input("fallback m·σ²", if rv.fallback { "yes" } else { "no" })
        .input(
            "ρ̂ (lag rows, pairs)",
            if rv.rho.is_empty() {
                "none".to_string()
            } else {
                rv.rho.iter().map(|(l, r, n)| format!("{l}:{r:+.3} ({n})")).collect::<Vec<_>>().join(" ")
            },
        )
        .note("conditional on the observed factor values: factor-forecast uncertainty is excluded")
        .note("assumes the window's innovations are independent of the training estimation error and the residual process is stable (σ² and ρ̂ from training)")
        .note("descriptive: no p-value, star or verdict — the window was chosen after the fact")
    } else {
        base_card(fit, a, "ATTR COEFFICIENT SE · attr.coef_se", "SE = √(dᵀVd), V the stored coefficient covariance")
            .note("total ≠ Σ term SEs (covariances)")
    };
    match ci.and_then(|i| a.covariances.get(i)) {
        Some(cv) => {
            c = c.input("estimator", cv.label.clone());
            let v = if predictive { cv.predictive_se } else { cv.coefficient_se };
            match (v, &cv.unavailable) {
                (Some(v), _) => c = c.value(format!("{} {}", unsigned(v * a.display_scale, p), a.display_unit)),
                (None, Some(why)) => c = c.unavailable(why.clone()),
                (None, None) => {
                    c = c.unavailable(rv.unavailable.clone().unwrap_or_else(|| "standard error unavailable".into()))
                }
            }
        }
        None => c = c.unavailable("coefficient SE unavailable: model saved before coefficient covariance was stored; Refit in Lab and save a new version"),
    }
    c
}

// ---- painting ------------------------------------------------------------------------------------

/// Ordered waterfall bars: (label, from, to, ink, dashed, card selector).
struct Bar {
    label: String,
    from: f64,
    to: f64,
    fill: Option<Color32>,
    outline: Color32,
    dashed: bool,
    card: usize,
}
const CARD_ACTUAL: usize = usize::MAX;
const CARD_DRIFT: usize = usize::MAX - 1;
const CARD_RESID: usize = usize::MAX - 2;
const CARD_PRED: usize = usize::MAX - 3;

fn bars(a: &AttributionReport) -> (Vec<Bar>, usize) {
    let mut v = vec![Bar {
        label: "Drift".into(),
        from: 0.,
        to: a.intercept,
        fill: Some(theme::muted().gamma_multiply(0.6)),
        outline: theme::muted(),
        dashed: false,
        card: CARD_DRIFT,
    }];
    let mut run = a.intercept;
    for (j, f) in a.factors.iter().enumerate() {
        let ticker = f.label.split(" · ").next().unwrap_or("").to_string();
        v.push(Bar {
            label: ticker,
            from: run,
            to: run + f.contribution,
            fill: Some(theme::series(j % 6)),
            outline: theme::text(),
            dashed: j >= 6,
            card: j,
        });
        run += f.contribution;
    }
    v.push(Bar { label: "Residual".into(), from: run, to: run + a.residual, fill: None, outline: theme::text(), dashed: false, card: CARD_RESID });
    let gap = v.len();
    v.push(Bar { label: "Predicted".into(), from: 0., to: a.predicted, fill: None, outline: theme::text(), dashed: false, card: CARD_PRED });
    v.push(Bar { label: "Actual".into(), from: 0., to: a.actual, fill: Some(theme::text().gamma_multiply(0.6)), outline: theme::text(), dashed: false, card: CARD_ACTUAL });
    (v, gap)
}

fn dashed_rect(p: &egui::Painter, r: Rect, stroke: Stroke) {
    let mut x = r.left();
    while x < r.right() {
        let e = (x + 3.).min(r.right());
        p.line_segment([pos2(x, r.top()), pos2(e, r.top())], stroke);
        p.line_segment([pos2(x, r.bottom()), pos2(e, r.bottom())], stroke);
        x += 6.;
    }
    let mut y = r.top();
    while y < r.bottom() {
        let e = (y + 3.).min(r.bottom());
        p.line_segment([pos2(r.left(), y), pos2(r.left(), e)], stroke);
        p.line_segment([pos2(r.right(), y), pos2(r.right(), e)], stroke);
        y += 6.;
    }
}

fn waterfall(ui: &mut Ui, id: PaneId, fit: &FrozenFit, a: &AttributionReport, st: &AttrUi, width: f32) {
    let (list, gap_at) = bars(a);
    let sc = a.display_scale;
    let p = precision(a);
    let (mut lo, mut hi) = (0f64, 0f64);
    for b in &list {
        lo = lo.min(b.from).min(b.to);
        hi = hi.max(b.from).max(b.to);
    }
    let pad = (hi - lo).max(1e-300) * 0.08;
    let (lo, hi) = (lo - pad, hi + pad);
    ui.horizontal(|ui| {
        ui.label(ui_text("WATERFALL", 10., theme::muted()));
        ui.label(ui_text(a.display_unit, 10., theme::muted()));
    });
    let (rect, _) = ui.allocate_exact_size(vec2(width, 240.), Sense::hover());
    #[cfg(test)]
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("attr-probe"), rect));
    let pt = ui.painter().with_clip_rect(rect);
    pt.rect_stroke(rect, 0., Stroke::new(1., theme::grid()), egui::StrokeKind::Inside);
    let plot = Rect::from_min_max(rect.min + vec2(0., 16.), rect.max - vec2(0., 20.));
    let y_of = |v: f64| plot.bottom() - plot.height() * ((v - lo) / (hi - lo).max(1e-300)) as f32;
    pt.hline(rect.x_range(), y_of(0.), Stroke::new(1., theme::grid()));
    let slots = list.len() + 1;
    let slot_w = rect.width() / slots as f32;
    let cov_ix = st.cov.filter(|i| *i < a.covariances.len()).or(a.default_cov);
    for (k, b) in list.iter().enumerate() {
        let x0 = rect.left() + slot_w * (k + usize::from(k >= gap_at)) as f32;
        let slot = Rect::from_min_max(pos2(x0, rect.top()), pos2(x0 + slot_w, rect.bottom()));
        let body = Rect::from_min_max(pos2(x0 + slot_w * 0.15, y_of(b.from.max(b.to) )), pos2(x0 + slot_w * 0.85, y_of(b.from.min(b.to))));
        let body = if body.height() < 1. { Rect::from_center_size(body.center(), vec2(body.width(), 1.)) } else { body };
        if let Some(f) = b.fill {
            pt.rect_filled(body, 0., f);
        }
        let stroke = Stroke::new(1., b.outline);
        if b.dashed {
            dashed_rect(&pt, body, stroke);
        } else if b.fill.is_none() || b.card == CARD_ACTUAL {
            pt.rect_stroke(body, 0., stroke, egui::StrokeKind::Inside);
        }
        let net = b.to - b.from;
        let txt = if b.card == CARD_PRED || b.card == CARD_ACTUAL { b.to } else { net };
        pt.text(
            pos2(body.center().x, if txt >= 0. { body.top() - 1. } else { body.bottom() + 1. }),
            if txt >= 0. { Align2::CENTER_BOTTOM } else { Align2::CENTER_TOP },
            signed(txt * sc, p),
            theme::mono(9.),
            theme::sign(txt),
        );
        pt.text(pos2(slot.center().x, rect.bottom() - 2.), Align2::CENTER_BOTTOM, &b.label, theme::ui_font(9.), theme::text());
        if b.card == CARD_DRIFT && a.mode == AttrMode::Endpoint {
            pt.text(pos2(slot.center().x, y_of(0.) - 12.), Align2::CENTER_BOTTOM, "0 · no drift in an endpoint Δ", theme::ui_font(8.), theme::muted());
        }
        #[cfg(test)]
        if b.card == 0 {
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("attr-bar0"), body));
        }
        let resp = ui.interact(slot, egui::Id::new(("attr-bar", id, k)), Sense::hover());
        resp.math_card(|| match b.card {
            CARD_DRIFT => drift_card(fit, a),
            CARD_RESID => residual_card(fit, a),
            CARD_PRED => se_card(fit, a, cov_ix, st.se == AttributionSe::Predictive),
            CARD_ACTUAL => actual_card(fit, a),
            j => contribution_card(fit, a, j, cov_ix),
        });
    }
}

/// Cumulative through-time lines from `path`; the final point equals the cross-section.
fn path_chart(ui: &mut Ui, a: &AttributionReport, width: f32) {
    if a.path.len() < 2 {
        return;
    }
    studio_heading(ui, "THROUGH TIME · cumulative");
    let (rect, _) = ui.allocate_exact_size(vec2(width, 160.), Sense::hover());
    let p = ui.painter().with_clip_rect(rect);
    p.rect_stroke(rect, 0., Stroke::new(1., theme::grid()), egui::StrokeKind::Inside);
    let n = a.path.len();
    let cols = (width as usize).clamp(2, n);
    let idx: Vec<usize> = (0..cols).map(|c| c * (n - 1) / (cols - 1)).collect();
    let k = a.factors.len();
    let series = |s: usize, q: &attr::AttrPathPoint| match s {
        0 => q.actual,
        1 => q.drift,
        s if s <= k + 1 => q.factors[s - 2],
        _ => q.residual,
    };
    let (mut lo, mut hi) = (0f64, 0f64);
    for &i in &idx {
        for s in 0..k + 3 {
            let v = series(s, &a.path[i]) * a.display_scale;
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    let span = (hi - lo).max(1e-300);
    let y_of = |v: f64| rect.bottom() - 3. - (rect.height() - 6.) * ((v * a.display_scale - lo) / span) as f32;
    let x_of = |c: usize| rect.left() + rect.width() * c as f32 / (cols - 1) as f32;
    p.hline(rect.x_range(), y_of(0.), Stroke::new(1., theme::grid()));
    for s in 1..k + 3 {
        let ink = match s {
            1 => theme::muted(),
            s if s <= k + 1 => theme::series((s - 2) % 6),
            _ => theme::text().gamma_multiply(0.7),
        };
        let pts: Vec<Pos2> = idx.iter().enumerate().map(|(c, &i)| pos2(x_of(c), y_of(series(s, &a.path[i])))).collect();
        p.add(egui::Shape::line(pts, Stroke::new(1., ink)));
    }
    let pts: Vec<Pos2> = idx.iter().enumerate().map(|(c, &i)| pos2(x_of(c), y_of(series(0, &a.path[i])))).collect();
    p.add(egui::Shape::line(pts, Stroke::new(2., theme::text())));
    p.text(rect.left_top() + vec2(4., 2.), Align2::LEFT_TOP, "actual (bold) · drift · factors · residual", theme::ui_font(9.), theme::muted());
}

fn ribbon(ui: &mut Ui, fit: &FrozenFit, a: &AttributionReport) {
    egui::Frame::new().fill(theme::card()).inner_margin(6.).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let mut tick = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(mono_text("FROZEN FIT · attributed, not refit", 11., theme::text()));
            ui.label(mono_text(&fit.id, 11., theme::text()));
            ui.label(mono_text(format!("trained thru {}", stamp(a.trained_thru)), 10., theme::muted()));
            if a.unavailable.is_none() {
                ui.label(mono_text(
                    format!("{} → {} · {}", stamp(a.from_ts), stamp(a.to_ts), mode_text(a)),
                    10.,
                    theme::text(),
                ));
            }
            let caution = !matches!(a.label, WindowLabel::OutOfSample);
            let r = ui.label(mono_text(label_text(&a.label), 10.5, if caution { theme::caution() } else { theme::text() }));
            if caution {
                tick = Some(r.rect);
            }
        });
        if let Some(r) = tick {
            caution_corner(ui, r);
        }
        ui.label(mono_text(
            format!("embargo {} bars · saved {} · digest {:08x}", a.embargo_bars, date(fit.saved_at), a.model_digest >> 32),
            10.,
            theme::muted(),
        ));
    });
}

fn equation(ui: &mut Ui, a: &AttributionReport) {
    let (sc, p) = (a.display_scale, precision(a));
    let factors = a.factor_total();
    let part = |ui: &mut Ui, name: &str, v: f64| {
        ui.label(mono_text(format!("{name} "), 13., theme::muted()));
        ui.label(mono_text(signed(v * sc, p), 13., theme::sign(v)));
    };
    let resp = ui
        .horizontal_wrapped(|ui| {
            part(ui, "Actual", a.actual);
            ui.label(mono_text("=", 13., theme::muted()));
            part(ui, "Drift", a.intercept);
            ui.label(mono_text("+", 13., theme::muted()));
            part(ui, "Factors", factors);
            ui.label(mono_text("+", 13., theme::muted()));
            part(ui, "Residual", a.residual);
            ui.label(mono_text(a.display_unit, 11., theme::muted()));
        })
        .response;
    resp.on_hover_ui(|ui| {
        for f in &a.factors {
            ui.label(mono_text(format!("{} {}", f.label, signed(f.contribution * sc, p)), 10., theme::sign(f.contribution)));
        }
    });
    if let Some(s) = a.simple_actual {
        ui.label(mono_text(
            format!("simple {} bp (expm1; not additively decomposed)", signed(s * 1e4, 1)),
            10.,
            theme::muted(),
        ));
    }
}

fn table(ui: &mut Ui, fit: &FrozenFit, a: &AttributionReport, st: &mut AttrUi) {
    let (sc, p) = (a.display_scale, precision(a));
    let ci = st.cov.filter(|i| *i < a.covariances.len()).or(a.default_cov);
    let cov = ci.and_then(|i| a.covariances.get(i));
    let unit = a.display_unit;
    let se_text = |v: Option<f64>| v.map_or("—".to_string(), |v| unsigned(v * sc, p));
    let mut new_hover = None;
    let focus = st.hover;
    egui::Grid::new("attr-table").num_columns(6).spacing([14., 3.]).striped(true).show(ui, |ui| {
        for h in [
            "Term · transform · lag".to_string(),
            "Σx (window)".to_string(),
            "β".to_string(),
            format!("Contribution [{unit}]"),
            format!("Coef SE [{unit}]"),
            "Move".to_string(),
        ] {
            ui.label(ui_text(h, 9.5, theme::muted()));
        }
        ui.end_row();
        // Drift.
        {
            let drift_se = match a.mode {
                AttrMode::Sum { .. } => se_text(cov.and_then(|c| c.intercept_se)),
                AttrMode::Endpoint => "0".into(),
            };
            let r = ui.label(mono_text("Drift (intercept)", 10.5, theme::text())).math_card(|| drift_card(fit, a));
            if focus == Some(0) {
                ui.painter().vline(r.rect.left() - 5., r.rect.y_range(), Stroke::new(2., theme::cyan()));
            }
            if r.hovered() {
                new_hover = Some(0);
            }
            let d0 = match a.mode {
                AttrMode::Sum { .. } => a.m as f64,
                AttrMode::Endpoint => 0.,
            };
            ui.label(mono_text(format!("d₀ = {d0:.0}"), 10.5, theme::text()));
            ui.label(mono_text(beta_text(fit.model.coefficients[0]), 10.5, theme::text()));
            ui.label(mono_text(signed(a.intercept * sc, p), 10.5, theme::sign(a.intercept)));
            ui.label(mono_text(drift_se, 10.5, theme::text()));
            ui.label(mono_text(if a.mode == AttrMode::Endpoint { "no drift in an endpoint Δ" } else { "—" }, 10., theme::muted()));
            ui.end_row();
        }
        for (j, f) in a.factors.iter().enumerate() {
            let (fs, funit) = scale_of(f.transform);
            let r = ui.label(mono_text(&f.label, 10.5, theme::series(j % 6))).math_card(|| contribution_card(fit, a, j, ci));
            if focus == Some(j + 1) {
                ui.painter().vline(r.rect.left() - 5., r.rect.y_range(), Stroke::new(2., theme::cyan()));
            }
            if r.hovered() {
                new_hover = Some(j + 1);
            }
            ui.label(mono_text(format!("{} {funit}", signed(f.exposure * fs, if fs == 1. { 4 } else { 1 }).replace('+', "")), 10.5, theme::text()));
            ui.label(mono_text(beta_text(f.beta), 10.5, theme::text()));
            ui.label(mono_text(signed(f.contribution * sc, p), 10.5, theme::sign(f.contribution)));
            ui.label(mono_text(se_text(ci.and_then(|i| f.coefficient_se.get(i).copied().flatten())), 10.5, theme::text()));
            match &f.kind {
                ExposureKind::Move { from_ts, to_ts } => {
                    ui.label(mono_text(format!("{} → {}", stamp(*from_ts), stamp(*to_ts)), 9.5, theme::muted()));
                }
                ExposureKind::EndpointMove => {
                    ui.label(mono_text("endpoint x_b − x_a", 9.5, theme::muted()));
                }
                ExposureKind::NotAMove { why } => {
                    let r = ui.label(mono_text(format!("N/A: {why}"), 9.5, theme::caution()));
                    caution_corner(ui, r.rect);
                }
            }
            ui.end_row();
        }
        let r = ui.label(mono_text("Residual", 10.5, theme::text())).math_card(|| residual_card(fit, a));
        if focus == Some(a.factors.len() + 1) {
            ui.painter().vline(r.rect.left() - 5., r.rect.y_range(), Stroke::new(2., theme::cyan()));
        }
        if r.hovered() {
            new_hover = Some(a.factors.len() + 1);
        }
        ui.label(mono_text("—", 10.5, theme::muted()));
        ui.label(mono_text("—", 10.5, theme::muted()));
        ui.label(mono_text(signed(a.residual * sc, p), 10.5, theme::sign(a.residual)));
        ui.label(mono_text(se_text(a.residual_var.variance.map(f64::sqrt)), 10.5, theme::text()));
        ui.label(mono_text("√Var_W (residual term)", 9.5, theme::muted()));
        ui.end_row();
        ui.label(mono_text("Predicted", 10.5, theme::text()));
        ui.label(mono_text("", 10., theme::muted()));
        ui.label(mono_text("", 10., theme::muted()));
        ui.label(mono_text(signed(a.predicted * sc, p), 10.5, theme::sign(a.predicted)));
        ui.label(mono_text(se_text(cov.and_then(|c| c.coefficient_se)), 10.5, theme::text()));
        ui.label(mono_text(
            format!("predictive SE {}", se_text(cov.and_then(|c| c.predictive_se))),
            9.5,
            theme::muted(),
        ));
        ui.end_row();
    });
    st.hover = new_hover;
}

fn tiles(ui: &mut Ui, fit: &FrozenFit, a: &AttributionReport, st: &AttrUi) {
    let (sc, p) = (a.display_scale, precision(a));
    let ci = st.cov.filter(|i| *i < a.covariances.len()).or(a.default_cov);
    let predictive = st.se == AttributionSe::Predictive;
    let cov = ci.and_then(|i| a.covariances.get(i));
    let se = cov.and_then(|c| if predictive { c.predictive_se } else { c.coefficient_se });
    let which = if predictive { "predictive SE" } else { "coefficient SE" };
    let (text, ink) = match se {
        Some(s) => (
            format!("{} ± {} {} ({which})", signed(a.predicted * sc, p), unsigned(s * sc, p), a.display_unit),
            theme::sign(a.predicted),
        ),
        None => {
            let why = cov
                .and_then(|c| c.unavailable.clone())
                .or_else(|| predictive.then(|| a.residual_var.unavailable.clone()).flatten())
                .unwrap_or_else(|| "unavailable".into());
            (format!("{} {} · {which} withheld · {why}", signed(a.predicted * sc, p), a.display_unit), theme::caution())
        }
    };
    ui.horizontal_wrapped(|ui| {
        let r = stat_tile(ui, "Predicted", &text, ink, || se_card(fit, a, ci, predictive));
        if se.is_none() {
            caution_corner(ui, r.rect);
        }
        stat_tile(ui, "Window", &format!("{} · {}", mode_text(a), label_text(&a.label)), theme::text(), || window_card(fit, a));
    });
}

fn caveats(ui: &mut Ui, a: &AttributionReport) {
    let line = |ui: &mut Ui, t: String| {
        let r = ui.label(mono_text(t, 10., theme::caution()));
        caution_corner(ui, r.rect);
    };
    if a.drift_led {
        line(ui, "DRIFT-LED: the intercept drives most of the prediction; read with care".into());
    }
    if a.label == WindowLabel::InSample {
        line(ui, "IN-SAMPLE: β was fitted on these rows; residuals are in-sample; the predictive SE is not an out-of-sample error".into());
    }
    for c in &a.caveats {
        line(ui, c.clone());
    }
    ui.label(ui_text("No causal reading: contributions are frozen exposure × observed factor move.", 10., theme::muted()));
}

fn controls(ui: &mut Ui, a: &AttributionReport, st: &mut AttrUi) {
    ui.horizontal_wrapped(|ui| {
        ui.label(ui_text("SE", 10., theme::muted()));
        for (v, l) in [(AttributionSe::Coefficient, "Coefficient"), (AttributionSe::Predictive, "Predictive")] {
            if ui.selectable_label(st.se == v, ui_text(l, 10.5, theme::text())).clicked() {
                st.se = v;
            }
        }
        if !a.covariances.is_empty() {
            ui.label(ui_text("estimator", 10., theme::muted()));
            let cur = st.cov.filter(|i| *i < a.covariances.len()).or(a.default_cov);
            for (i, c) in a.covariances.iter().enumerate() {
                let r = ui.selectable_label(cur == Some(i), mono_text(&c.label, 10., theme::text()));
                if r.clicked() {
                    st.cov = Some(i);
                }
                if c.coefficient_se.is_none() && c.unavailable.is_some() {
                    caution_corner(ui, r.rect);
                }
            }
        }
    });
}

/// The attribution body: ribbon, equation, waterfall, table, tiles, through-time chart, caveats.
pub(crate) fn panel(ui: &mut Ui, id: PaneId, fit: &FrozenFit, a: &AttributionReport, st: &mut AttrUi) {
    let width = ui.available_width();
    ribbon(ui, fit, a);
    if let Some(why) = &a.unavailable {
        egui::Frame::new().fill(theme::card()).inner_margin(8.).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let r = ui.label(mono_text(format!("ATTRIBUTION UNAVAILABLE · {why}"), 11., theme::caution()));
            caution_corner(ui, r.rect);
            ui.label(ui_text("Nothing is interpolated, carried, skipped or compressed.", 10., theme::muted()));
        });
        return;
    }
    controls(ui, a, st);
    equation(ui, a);
    waterfall(ui, id, fit, a, st, width);
    studio_heading(ui, "TERMS");
    table(ui, fit, a, st);
    tiles(ui, fit, a, st);
    path_chart(ui, a, width);
    caveats(ui, a);
    // One line; the full receipt text is on hover.
    let parts = crate::widgets::chip_parts(&a.source_receipts);
    crate::widgets::source_chip(ui, a.provenance.label(), &parts, || {
        format!(
            "{} · CLOSED THRU {} · {} · {}",
            a.provenance.label(),
            stamp(a.closed_thru),
            clock(a, fit.model.tf == Timeframe::D1),
            a.source
        )
    });
}

fn reserve(ui: &mut Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 240. + 160.), Sense::hover());
    ui.painter().rect_stroke(rect, 0., Stroke::new(1., theme::grid()), egui::StrokeKind::Inside);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, theme::ui_font(12.), theme::muted());
}

fn list_view(ui: &mut Ui, pane: &AttrPane) -> Option<String> {
    let mut open = None;
    if let Some(e) = &pane.list_error {
        ui.label(ui_text(format!("Could not list saved models: {e}"), 11., theme::down()));
    }
    match &pane.listing {
        None if pane.listing_loading => {
            ui.label(ui_text("Loading saved models…", 11., theme::muted()));
        }
        Some(l) if l.is_empty() => {
            ui.label(ui_text("No saved residual models yet. Open Regression Lab (STUDIO lab), fit a model, then Save residual · STUDIO attr model=<id>", 11., theme::muted()));
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
            ui.label(ui_text("Open a saved model: STUDIO attr model=<id>", 11., theme::muted()));
        }
    }
    open
}

pub(super) fn show(ui: &mut Ui, id: PaneId, pane: &mut AttrPane, worker: &crate::worker::Worker, sample: bool) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut reload = false;
    let mut new_window: Option<AttrWindow> = None;
    ui.horizontal_wrapped(|ui| {
        if pane.listing.as_ref().is_some_and(|l| l.is_empty()) && ui.small_button("Open Regression Lab").clicked() {
            actions.push(Action::Command("STUDIO lab".into()));
        }
        studio_title(ui, "ATTRIBUTION");
        if let Some(m) = &pane.model {
            ui.label(mono_text(
                format!("{} · {} · {}", tr_data::security(&m.model.symbol).ticker, m.model.tf.label(), m.id),
                11.,
                theme::text(),
            ));
        } else if !pane.model_id.is_empty() {
            ui.label(mono_text(&pane.model_id, 11., theme::text()));
        }
        reload |= ui.small_button("Reload").on_hover_text("Re-read every series, rescore and reattribute").clicked();
        if pane.model.is_some() {
            if ui.small_button("Copy command").on_hover_text("Copy a command that reopens this attribution").clicked() {
                ui.ctx().copy_text(format!("STUDIO attr model={} {}", pane.model_id, pane.window.words(pane.is_daily())));
            }
            let to = match &pane.window {
                AttrWindow::Obs { to, .. } | AttrWindow::Range { to, .. } => *to,
            };
            for n in [1usize, 5, 20, 60] {
                let on = matches!(&pane.window, AttrWindow::Obs { obs, .. } if *obs == n);
                if ui.selectable_label(on, mono_text(format!("{n} obs"), 10.5, theme::text())).clicked() {
                    new_window = Some(AttrWindow::Obs { obs: n, to });
                }
            }
            if let AttrWindow::Range { .. } = &pane.window {
                ui.label(mono_text(format!("Range {}", pane.window.words(pane.is_daily())), 10.5, theme::text()));
            }
        }
    });
    if sample {
        pane.clear_for_sample();
    } else {
        pane.sample_text = None;
        if reload {
            pane.reload();
        }
        if let Some(w) = new_window {
            pane.set_window(w);
        }
        pane.drive(id, worker);
    }
    let mut open: Option<String> = None;
    egui::ScrollArea::vertical().id_salt(("attr-scroll", id)).auto_shrink([false, false]).show(ui, |ui| {
        if let Some(t) = pane.sample_text.clone() {
            ui.label(ui_text(t, 12., theme::caution()));
            return;
        }
        if let Some(u) = pane.unavailable.clone() {
            super::studio_residual::model_unavailable(ui, &u);
            open = list_view(ui, pane);
            return;
        }
        if pane.model_id.is_empty() {
            ui.label(ui_text("ATTRIBUTION · actual = drift + factor contributions + residual, from a saved frozen model", 11., theme::text()));
            ui.label(ui_text("STUDIO attr model=<id> obs=20", 10., theme::muted()));
            open = list_view(ui, pane);
            return;
        }
        let Some(model) = pane.model.clone() else {
            reserve(ui, "Loading model…");
            return;
        };
        if let Some(r) = &pane.report {
            badges(ui, r);
        }
        if let Some(a) = pane.attribution.clone() {
            panel(ui, id, &model, &a, &mut pane.ui);
        } else if let Some(prev) = pane.previous.clone().filter(|_| pane.attr_error.is_none() && pane.error.is_none()) {
            ui.label(ui_text(format!("PREVIOUS SNAPSHOT · {}", prev.window.words(model.model.tf == Timeframe::D1)), 11., theme::muted()));
            panel(ui, id, &model, &prev, &mut pane.ui);
        } else if let Some(e) = pane.error.clone().or_else(|| pane.attr_error.clone()) {
            egui::Frame::new().fill(theme::card()).inner_margin(8.).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let r = ui.label(ui_text(format!("ATTRIBUTION UNAVAILABLE · {e}"), 12., theme::caution()));
                caution_corner(ui, r.rect);
            });
        } else if let Some(why) = pane.report.as_ref().and_then(|r| r.unavailable.clone()) {
            egui::Frame::new().fill(theme::card()).inner_margin(8.).show(ui, |ui| {
                ui.label(ui_text(format!("ATTRIBUTION UNAVAILABLE · score unavailable: {why}"), 12., theme::caution()));
            });
        } else if pane.scoring {
            reserve(ui, &format!("Scoring {} series…", 1 + model.model.factors.len()));
        } else {
            reserve(ui, "Attributing…");
        }
    });
    if let Some(id) = open {
        pane.open(id);
    }
    actions
}

// ---- fixtures (tests and the paint gate only) ------------------------------------------------------

/// Dev/test fixture: a frozen 7-factor model scored on its own live rows.
#[cfg(test)]
pub(super) fn fixture(n: usize) -> (Arc<FrozenFit>, Arc<ResidualReport>) {
    let (fit, rep, _) = super::studio_residual::fixture(n);
    (fit, rep)
}

#[cfg(test)]
mod tests;
