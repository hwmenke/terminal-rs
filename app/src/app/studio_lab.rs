//! Regression Lab pane (Studio slice 5): factor editor, fitted-vs-actual and residual plots, the
//! coefficient table with four covariance estimates, the purged frozen holdout, diagnostics and the
//! MathCards for every derived statistic. The maths lives in `tr_data::studio_lab`; this module
//! loads real verified rows (shared bounded loader), keys the report, and paints immutable `Arc`s.
use super::studio_corr::{CorrCache, load_rows, relate_ui::signed};
use super::studio_dist::stamp;
use super::{PaneKind, TerminalApp};
use crate::widgets::{HeatScale, MathCard, MathCardExt, stat_tile};
use crate::{congress_view::Action, dock::PaneId, theme, worker::Request};
use egui::{Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use std::sync::{Arc, LazyLock};
use tr_core::{Bar, Timeframe};
use tr_data::studio::Transform;
use tr_data::studio_corr::{NamedBars, SE_METHODS};
use tr_data::studio_lab::{
    self as lab, CoefInference, LabArgs, LabReport, LabSpec, Segment, FactorSpec, MAX_FACTORS, MAX_LAG,
};
use tr_data::studio_inference::CovarianceMethod;

/// Reports are small (≤ 5,000 points); keep the 8 most recent.
static LAB_REPORTS: LazyLock<tr_data::report_cache::ReportCache> =
    LazyLock::new(|| tr_data::report_cache::ReportCache::new(8));

const COV_CHIPS: [&str; 4] = ["Classical", "HC3", "HAC 5", "HAC auto"];
const COV_NOTES: [&str; 4] = [
    "Classical: assumes homoskedastic, serially uncorrelated errors; overlapping horizons overstate precision.",
    "HC3: robust to heteroskedasticity only; ignores serial dependence.",
    "HAC 5: Newey–West Bartlett, fixed lag 5 (uniform h−1 kernel when horizons overlap).",
    "HAC auto: Newey–West Bartlett, automatic lag (uniform h−1 kernel when horizons overlap); the default.",
];

// ---- pane ---------------------------------------------------------------------------------------

/// Ephemeral view state. Nothing here ever triggers a request.
#[derive(Default)]
pub struct LabUi {
    pub shown_cov: usize,
    pub diag_tab: usize,
    env: Option<(u64, usize, Arc<Envelope>)>,
}

pub struct LabPane {
    pub symbol: String,
    pub tf: Timeframe,
    pub spec: LabSpec,
    /// Editor buffer; becomes `spec` only on Apply.
    pub draft: LabSpec,
    /// Ticker text per draft factor (the `TextEdit` buffers).
    pub draft_syms: Vec<String>,
    pub token: u64,
    pub loading: bool,
    pub error: Option<String>,
    pub report: Option<Arc<LabReport>>,
    /// The last accepted report while a changed-spec request is in flight.
    pub previous: Option<Arc<LabReport>>,
    pub ui: LabUi,
    /// Save residual: the pending request token and the last message.
    pub save: super::studio_residual::SaveState,
    add_text: String,
    fresh: bool,
}

fn cov_index(m: CovarianceMethod) -> usize {
    SE_METHODS.iter().position(|s| *s == m).unwrap_or(3)
}

impl LabPane {
    fn save_disabled_reason(&self, sample: bool) -> Option<&'static str> {
        if sample { Some("Save requires verified real data; sample mode cannot save a residual model.") }
        else if self.save.pending.is_some() { Some("Saving this residual model; wait for the save to finish.") }
        else if !self.report.as_ref().is_some_and(|r| r.fit.is_some()) {
            Some("Save requires a successful verified-data fit. Reload to retry the data sources, then fit the model in Regression Lab.")
        } else { None }
    }

    pub fn new(symbol: impl Into<String>, tf: Timeframe, spec: LabSpec) -> Self {
        let symbol = symbol.into();
        let mut spec = spec;
        if spec.factors.is_empty() && !symbol.is_empty() {
            spec.factors = LabSpec::default_for(&symbol).factors;
        }
        spec.target = spec.target.with_valid_horizon();
        for f in &mut spec.factors {
            f.transform = f.transform.with_valid_horizon();
            f.lag = f.lag.min(MAX_LAG);
        }
        spec.holdout_pct = spec.holdout_pct.clamp(10, 40);
        let ui = LabUi { shown_cov: cov_index(spec.covariance), ..Default::default() };
        let mut pane = Self {
            symbol,
            tf,
            draft: spec.clone(),
            spec,
            draft_syms: vec![],
            token: 0,
            loading: false,
            error: None,
            report: None,
            previous: None,
            ui,
            save: Default::default(),
            add_text: String::new(),
            fresh: false,
        };
        pane.sync_syms();
        pane
    }
    fn sync_syms(&mut self) {
        self.draft_syms = self.draft.factors.iter().map(|f| tr_data::security(&f.symbol).ticker).collect();
    }
    /// Orphans any in-flight response. `keep_previous` shows the old report as PREVIOUS SNAPSHOT.
    pub fn invalidate(&mut self, keep_previous: bool) {
        self.token = crate::worker::studio_token();
        self.loading = false;
        self.error = None;
        self.previous = if keep_previous { self.report.take().or(self.previous.take()) } else { None };
        self.report = None;
    }
    /// Applies the draft: new spec, new token, the previous report kept as a snapshot.
    pub fn apply(&mut self) -> Result<(), String> {
        for (f, s) in self.draft.factors.iter_mut().zip(&self.draft_syms) {
            let word = s.trim();
            if word.is_empty() {
                return Err("every factor needs a ticker".into());
            }
            f.symbol = tr_data::security(word).key();
        }
        self.draft.validate_for(&self.symbol)?;
        let changed = self.draft.key_string() != self.spec.key_string();
        self.spec = self.draft.clone();
        self.invalidate(changed);
        Ok(())
    }
    pub fn set_spec(&mut self, spec: LabSpec) {
        self.draft = spec;
        self.sync_syms();
        self.ui.shown_cov = cov_index(self.draft.covariance);
        let _ = self.apply();
        if self.draft.validate_for(&self.symbol).is_err() {
            self.spec = self.draft.clone();
            self.invalidate(false);
        }
    }
    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        self.token = crate::worker::studio_token();
        self.loading = false;
        self.error = None;
        if self.symbol.trim().is_empty() {
            self.error = Some("Regression Lab needs a ticker: AAPL STUDIO lab factors=SPY,TLT. Sample prices are never used.".into());
            return;
        }
        if let Err(e) = self.spec.validate_for(&self.symbol) {
            self.error = Some(e);
            return;
        }
        self.loading = true;
        worker.send(Request::StudioLab {
            pane,
            token: self.token,
            symbol: tr_data::security(&self.symbol).key(),
            tf: self.tf,
            spec: self.spec.clone(),
            fresh: std::mem::take(&mut self.fresh),
        });
    }
    /// Sample / offline mode: nothing is shown and nothing is requested.
    pub fn clear_for_sample(&mut self) {
        self.report = None;
        self.previous = None;
        self.loading = false;
        self.error = Some("Sample / offline: no provider calls. Regression Lab never uses sample prices.".into());
    }
    /// Accept rule: matching token, spec (the displayed covariance is not part of identity), symbol and tf.
    pub fn accept(&mut self, token: u64, spec: &LabSpec, result: Result<Arc<LabReport>, String>) {
        if token != self.token || spec.key_string() != self.spec.key_string() {
            return;
        }
        self.loading = false;
        self.report = None;
        self.previous = None;
        match result {
            Ok(r) => {
                if r.symbol != tr_data::security(&self.symbol).key() || r.tf != self.tf {
                    return;
                }
                match r.provenance.require_verified("Regression Lab") {
                    Ok(()) => {
                        self.error = None;
                        self.report = Some(r);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Err(e) => self.error = Some(e),
        }
    }
}

// ---- research (worker side) ---------------------------------------------------------------------

/// Loads the target and every factor through the shared bounded loader, then computes (or reuses)
/// the report. A failed or unadmitted series stays in the report as a `factor_error`; it is never
/// dropped, substituted or fetched from a fallback source. Sample mode makes no provider calls.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn research<F, Fut>(
    cache: &CorrCache,
    fresh: bool,
    jobs: Option<Arc<crate::perf::JobLog>>,
    history: &tr_data::history::Service,
    bridge: &tr_data::whats_news::Bridge,
    sample: bool,
    symbol: &str,
    tf: Timeframe,
    spec: &LabSpec,
    load_intraday: F,
) -> anyhow::Result<Arc<LabReport>>
where
    F: Fn(String, Timeframe) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<(Vec<Bar>, tr_data::vault::StitchInfo)>>,
{
    if sample {
        anyhow::bail!("Regression Lab: sample / offline mode makes no provider calls. Sample prices are never used.");
    }
    if tf != Timeframe::D1 && !tf.is_intraday() {
        anyhow::bail!("Regression Lab supports daily or intraday bars.");
    }
    let target_key = tr_data::security(symbol).key();
    spec.validate_for(&target_key).map_err(anyhow::Error::msg)?;
    let mut names = vec![target_key.clone()];
    names.extend(spec.factors.iter().map(|f| f.symbol.clone()));
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
    let (spec, reports) = (spec.clone(), &LAB_REPORTS);
    let report = tokio::task::spawn_blocking(move || {
        let _job = jobs.as_ref().map(|j| crate::perf::JobTimer::new(j, "StudioLab"));
        let key = lab::cache_key(&target, &rows, &spec, tf, &source, daily, observed_at, closed);
        let params = key.params;
        reports.get_or_compute(key, || {
            Ok::<_, String>(lab::compute(&target, &rows, &spec, tf, daily, &source, closed, observed_at, params))
        })
    })
    .await?
    .map_err(anyhow::Error::msg)?;
    Ok(report)
}

// ---- commands -----------------------------------------------------------------------------------

impl TerminalApp {
    /// `STUDIO lab …`: model edits go to a focused Lab when no interval or symbol is named;
    /// everything else opens (or replaces) the Lab pane.
    pub(super) fn open_lab(&mut self, symbol: Option<&str>, spec: &str) {
        let args = match LabArgs::parse(spec) {
            Ok(a) => a,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let focus = self.focus;
        if symbol.is_none() && !spec.trim().is_empty() && args.is_edit_only() {
            if let Some(PaneKind::StudioLab(p)) = self.panes.iter_mut().find(|p| p.id == focus).map(|p| &mut p.kind) {
                let mut next = p.draft.clone();
                match args.apply(&mut next) {
                    Ok(()) => {
                        p.set_spec(next);
                        self.status = "Regression Lab · model edited".into();
                    }
                    Err(e) => self.status = e,
                }
                return;
            }
        }
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        let mut model = if symbol.is_empty() { LabSpec::default() } else { LabSpec::default_for(&symbol) };
        if let Err(e) = args.apply(&mut model) {
            self.status = e;
            return;
        }
        self.status = "Regression Lab · verified real data · purged frozen holdout".into();
        self.open_panel(PaneKind::StudioLab(Box::new(LabPane::new(symbol, args.tf.unwrap_or(Timeframe::D1), model))));
    }
    pub(super) fn studio_lab_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("lab pane");
        let PaneKind::StudioLab(pane) = &mut self.panes[i].kind else { return vec![] };
        show(ui, id, pane, &self.worker, self.provider.eq_ignore_ascii_case("sample"))
    }
}

// ---- pane UI ------------------------------------------------------------------------------------

fn ui_text(s: impl Into<String>, size: f32, ink: Color32) -> RichText {
    RichText::new(s).font(theme::ui_font(size)).color(ink)
}
fn mono_text(s: impl Into<String>, size: f32, ink: Color32) -> RichText {
    RichText::new(s).font(theme::mono(size)).color(ink)
}

fn transform_combo(ui: &mut Ui, salt: impl std::hash::Hash + std::fmt::Debug, t: &mut Transform) {
    let h = t.horizon();
    let name = |t: &Transform| match t {
        Transform::Level => "level",
        Transform::LogLevel => "ln level",
        Transform::Change { .. } => "change",
        Transform::LogReturn { .. } => "log-ret",
    };
    egui::ComboBox::from_id_salt(salt).width(72.).selected_text(name(t)).show_ui(ui, |ui| {
        for next in [Transform::Level, Transform::LogLevel, Transform::Change { h }, Transform::LogReturn { h }] {
            if ui.selectable_label(std::mem::discriminant(t) == std::mem::discriminant(&next), name(&next)).clicked() {
                *t = next;
            }
        }
    });
    ui.add_enabled_ui(!t.is_level(), |ui| {
        for hh in [1usize, 5, 20] {
            let on = !t.is_level() && t.horizon() == hh;
            if ui.selectable_label(on, RichText::new(hh.to_string()).font(theme::mono(10.))).clicked() && !on {
                *t = match *t {
                    Transform::Change { .. } => Transform::Change { h: hh },
                    _ => Transform::LogReturn { h: hh },
                };
            }
        }
    });
}

/// Returns true when Apply was pressed (or Enter in a symbol box).
fn editor(ui: &mut Ui, id: PaneId, pane: &mut LabPane) -> bool {
    let mut apply = false;
    ui.label(ui_text("MODEL", 10., theme::muted()));
    ui.horizontal_wrapped(|ui| {
        ui.label(ui_text("Target", 11., theme::muted()));
        ui.label(mono_text(tr_data::security(&pane.symbol).ticker, 11., theme::text()));
        transform_combo(ui, (id, "target"), &mut pane.draft.target);
    });
    let mut remove = None;
    for i in 0..pane.draft.factors.len().min(pane.draft_syms.len()) {
        ui.horizontal_wrapped(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut pane.draft_syms[i]).desired_width(56.).font(theme::mono(11.)));
            apply |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            transform_combo(ui, (id, "f", i), &mut pane.draft.factors[i].transform);
            ui.label(ui_text("lag", 10., theme::muted()));
            ui.add(egui::DragValue::new(&mut pane.draft.factors[i].lag).range(0..=MAX_LAG).speed(0.2));
            if ui.small_button("✕").on_hover_text("Remove factor").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        pane.draft.factors.remove(i);
        pane.draft_syms.remove(i);
    }
    if pane.draft.factors.len() < MAX_FACTORS {
        ui.horizontal_wrapped(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut pane.add_text).desired_width(56.).hint_text("+ factor"));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (enter || ui.small_button("+ factor").clicked()) && !pane.add_text.trim().is_empty() {
                let sym = pane.add_text.trim().to_ascii_uppercase();
                pane.draft.factors.push(FactorSpec::new(&sym, Transform::LogReturn { h: 1 }, 0));
                pane.draft_syms.push(sym);
                pane.add_text.clear();
            }
        });
        let used: Vec<String> = std::iter::once(tr_data::security(&pane.symbol).ticker)
            .chain(pane.draft_syms.iter().map(|s| s.to_ascii_uppercase()))
            .collect();
        ui.horizontal_wrapped(|ui| {
            for t in tr_data::studio_corr::DEFAULT_SET.iter().filter(|t| !used.contains(&t.to_string())).take(4) {
                if ui.small_button(*t).clicked() && pane.draft.factors.len() < MAX_FACTORS {
                    pane.draft.factors.push(FactorSpec::new(t, Transform::LogReturn { h: 1 }, 0));
                    pane.draft_syms.push(t.to_string());
                }
            }
        });
    }
    ui.horizontal(|ui| {
        ui.label(ui_text("Holdout %", 11., theme::muted()));
        ui.add(egui::DragValue::new(&mut pane.draft.holdout_pct).range(10..=40).speed(0.2));
    });
    if ui.button("Apply").on_hover_text("Fit this model (edits never refit per keystroke)").clicked() {
        apply = true;
    }
    apply
}

pub(super) fn show(ui: &mut Ui, id: PaneId, pane: &mut LabPane, worker: &crate::worker::Worker, sample: bool) -> Vec<Action> {
    let mut reload = false;
    let mut save: Option<(u64, Arc<LabReport>)> = None;
    ui.horizontal_wrapped(|ui| {
        super::studio_dist::studio_title(ui, "REGRESSION LAB");
        ui.label(mono_text(
            format!("{} · {}", tr_data::security(&pane.symbol).ticker, pane.tf.label()),
            11.,
            theme::text(),
        ));
        if ui.small_button("Copy command").on_hover_text("Copy a STUDIO lab command that rebuilds this model").clicked() {
            ui.ctx().copy_text(format!("STUDIO lab {} {}", pane.tf.label(), pane.spec.command()));
        }
        reload = ui.small_button("Reload").on_hover_text("Re-read every series, bypassing the short bar cache").clicked();
        let disabled_reason = pane.save_disabled_reason(sample);
        let can_save = disabled_reason.is_none();
        if ui
            .add_enabled(can_save, egui::Button::new("Save residual").small())
            .on_disabled_hover_text(disabled_reason.unwrap_or_default())
            .on_hover_text("Freeze this fit into an immutable residual model (STUDIO resid open <id>)")
            .clicked()
            && let Some(r) = pane.report.clone()
        {
            let token = crate::worker::studio_token();
            pane.save = super::studio_residual::SaveState { pending: Some(token), msg: None };
            save = Some((token, r));
        }
        if let Some(m) = &pane.save.msg {
            ui.label(ui_text(m, 10., theme::muted()));
        }
    });
    if sample {
        pane.clear_for_sample();
    } else if reload {
        pane.fresh = true;
        pane.invalidate(false);
    }
    if !sample && pane.symbol.trim().is_empty() {
        pane.error = Some(
            "Regression Lab needs a ticker: AAPL STUDIO lab factors=SPY,TLT. Sample prices are never used.".into(),
        );
    } else if !sample && !pane.loading && pane.report.is_none() && pane.error.is_none() {
        pane.request(id, worker);
    }
    let wide = ui.available_width() >= 900.;
    let mut apply = false;
    egui::ScrollArea::vertical().id_salt(("lab-scroll", id)).auto_shrink([false, false]).show(ui, |ui| {
        let content = |ui: &mut Ui, pane: &mut LabPane| {
            if let Some(r) = pane.report.clone() {
                receipt(ui, &r);
                panel(ui, id, &r, &mut pane.ui);
            } else if let (true, Some(prev)) = (pane.loading, pane.previous.clone()) {
                ui.label(
                    ui_text(
                        format!("PREVIOUS SNAPSHOT · {} · recomputing…", prev.spec.command()),
                        11.,
                        theme::muted(),
                    ),
                );
                receipt(ui, &prev);
                panel(ui, id, &prev, &mut pane.ui);
            } else if pane.loading {
                let n = 1 + pane.spec.factors.len();
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 420.), Sense::hover());
                ui.painter().rect_stroke(rect, 0., Stroke::new(1., theme::grid()), egui::StrokeKind::Inside);
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    format!("Loading {n} series…"),
                    theme::ui_font(12.),
                    theme::muted(),
                );
            } else if let Some(e) = &pane.error {
                egui::Frame::new().fill(theme::card()).inner_margin(8.).show(ui, |ui| {
                    ui.label(ui_text(e, 12., theme::down()));
                });
            }
        };
        if wide {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(260.);
                    apply = editor(ui, id, pane);
                });
                ui.vertical(|ui| content(ui, pane));
            });
        } else {
            apply = editor(ui, id, pane);
            content(ui, pane);
        }
    });
    if apply && !sample {
        if let Err(e) = pane.apply() {
            pane.error = Some(e);
            pane.report = None;
            pane.previous = None;
        }
    }
    if let Some((token, r)) = save {
        worker.send(Request::SaveStudioFit {
            pane: id,
            token,
            name: None,
            monitor: None,
            source: tr_data::studio_residual::FitSource::Lab(r),
        });
    }
    vec![]
}

fn clock(r: &LabReport) -> String {
    match r.observed_at {
        Some(t) if r.tf == Timeframe::D1 => format!("{} (archive load time)", stamp(t)),
        Some(t) => stamp(t),
        None => "observation clock unavailable".into(),
    }
}

fn receipt(ui: &mut Ui, r: &LabReport) {
    ui.label(mono_text(
        format!(
            "{} · CLOSED THRU {} · {} · {}",
            r.provenance.label(),
            stamp(r.closed_thru),
            clock(r),
            r.source
        ),
        10.,
        theme::muted(),
    ));
}

// ---- MathCards ----------------------------------------------------------------------------------

fn units_note(r: &LabReport) -> Option<String> {
    let mut seen: Vec<&str> = vec![];
    for u in &r.units {
        if !seen.contains(&u.label()) {
            seen.push(u.label());
        }
    }
    (seen.len() > 1).then(|| r.units.iter().map(|u| u.label()).collect::<Vec<_>>().join(" | "))
}

fn has_level(r: &LabReport) -> bool {
    r.spec.target.is_level() || r.spec.factors.iter().any(|f| f.transform.is_level())
}

fn base_card(r: &LabReport, shown: usize, title: &str, formula: &str, n: usize, unit: &'static str) -> MathCard {
    let est = r.inference_at(shown).map_or_else(|| "covariance unavailable".to_string(), |i| i.covariance.label());
    let mut c = MathCard::new(title, formula)
        .n(n, unit)
        .closed_thru(stamp(r.closed_thru))
        .source(&r.source)
        .provenance(r.provenance)
        .input("estimator", est)
        .input("observation clock", clock(r))
        .input("target", format!("{} ({})", r.spec.target.short(), r.units.first().map_or("", |u| u.label())));
    if let Some((a, b)) = r.span {
        c = c.span(stamp(a), stamp(b));
    }
    if let Some(u) = units_note(r) {
        c = c.input("units (y | x…)", u);
    }
    if has_level(r) {
        c = c.caveat("levels may be nonstationary; spurious-regression risk");
    }
    if let Some(why) = &r.unavailable {
        c = c.unavailable(why.clone());
    }
    c
}

fn term_name(r: &LabReport, j: usize) -> String {
    if j == 0 { "intercept".into() } else { LabSpec::term_label(&r.spec.factors[j - 1]) }
}

fn coef_card(r: &LabReport, shown: usize, j: usize) -> MathCard {
    let fit = r.fit.as_ref().expect("fit");
    let inf = r.inference_at(shown);
    let mut c = base_card(
        r,
        shown,
        "LAB COEF · lab.fit.coef",
        "β = R⁻¹Qᵀy on training rows (pivoted QR, standardised columns); SE=√V_jj; z=β/SE; p=2(1−Φ(|z|)); q=BY over slopes",
        fit.n_train,
        "training rows",
    )
    .input("term", term_name(r, j))
    .input("β", signed(fit.coefficients[j], 6));
    if j > 0 {
        c = c.input("training μ / s", format!("{:.6e} / {:.6e}", fit.mean[j - 1], fit.scale[j - 1])).input(
            "VIF",
            format!("{:.2}", fit.vif.get(j - 1).copied().unwrap_or(f64::NAN)),
        );
    }
    match inf {
        Some(i) => {
            c = c.input("covariance", i.covariance.label());
            match (i.se[j], i.interval[j]) {
                (Some(se), Some((lo, hi))) => {
                    c = c.input("SE", format!("{se:.6e}")).input("95% CI", format!("{} … {}", signed(lo, 6), signed(hi, 6)));
                    if let Some(z) = i.z[j] {
                        c = c.input("z", signed(z, 3));
                    }
                    if let Some(p) = i.p[j] {
                        c = c.input("p", format!("{p:.3}"));
                    }
                    if let Some(q) = i.q[j] {
                        c = c.input("q (BY)", format!("{q:.3}"));
                    }
                }
                _ => {
                    c = c.unavailable(i.covariance.unavailable.clone().unwrap_or_else(|| "standard error unavailable".into()));
                }
            }
        }
        None => c = c.note(r.low_n.clone().unwrap_or_else(|| "inference withheld".into())),
    }
    c = more_caveat(
        c,
        "Descriptive full-sample fit; recomputation with new bars moves the split. Strength, not colour, marks significance.",
    );
    c.value(signed(fit.coefficients[j], 4))
}

fn sigma_card(r: &LabReport, shown: usize) -> MathCard {
    let f = r.fit.as_ref().expect("fit");
    base_card(r, shown, "LAB SIGMA · lab.fit.sigma", "σ = √(SSE/(n−k)) on training rows", f.n_train, "training rows")
        .input("n, k", format!("{}, {}", f.n_train, f.k))
        .input("training mean ȳ", format!("{:.6e}", f.train_mean))
        .value(format!("{:.6e}", f.residual_sigma))
}

fn r2_in_card(r: &LabReport, shown: usize) -> MathCard {
    let f = r.fit.as_ref().expect("fit");
    let mut c = base_card(
        r,
        shown,
        "LAB R² IN · lab.fit.r2_in",
        "R² = 1 − SSE/Σ(y−ȳ)²; adj = 1 − (1−R²)(n−1)/(n−k); in-sample, descriptive",
        f.n_train,
        "training rows",
    );
    if let Some(a) = f.r2_adj {
        c = c.input("adjusted R²", signed(a, 4));
    }
    match f.r2_in {
        Some(v) => c.value(signed(v, 4)),
        None => c.unavailable("training targets have no variance"),
    }
}

fn oos_card(r: &LabReport, shown: usize) -> MathCard {
    let (f, h) = (r.fit.as_ref().expect("fit"), r.holdout.as_ref().expect("holdout"));
    let mut c = base_card(
        r,
        shown,
        "LAB R² OOS · lab.holdout.r2_oos",
        "R²_OOS = 1 − Σtest(y−ŷ)² / Σtest(y−ȳ_train)²  (frozen training β, μ, s, ȳ)",
        h.n_test,
        "test rows",
    )
    .input("train / purged / test", format!("{} / {} / {}", f.n_train, h.purged, h.n_test))
    .input("split", stamp(f.split_ts))
    .input("training ȳ", format!("{:.6e}", f.train_mean))
    .input("SSE / baseline", format!("{:.6e} / {:.6e}", h.sse, h.baseline_sse))
    .note(format!("split at {}% of current complete rows; recomputation with new bars moves the split.", 100 - r.spec.holdout_pct));
    if !h.forecast_style {
        c = c.caveat("contemporaneous / overlapping explanatory fit — not a forecast");
    }
    match h.r2_oos {
        Some(v) => c.value(signed(v, 4)),
        None => c.unavailable(h.unavailable.clone().unwrap_or_else(|| "unavailable".into())),
    }
}

fn condition_card(r: &LabReport, shown: usize) -> MathCard {
    let f = r.fit.as_ref().expect("fit");
    base_card(
        r,
        shown,
        "LAB CONDITION · lab.diag.condition",
        "κ∞(R) = ‖R‖∞·‖R⁻¹‖∞ on the standardised design; refuse if κ > 10⁶ or |R_ii| ≤ 1e-7·|R_00|",
        f.n_train,
        "training rows",
    )
    .input("rank / k", format!("{} / {}", f.rank, f.k))
    .value(format!("{:.3e}", f.kappa_inf))
}

fn vif_card(r: &LabReport, shown: usize, j: usize) -> MathCard {
    let f = r.fit.as_ref().expect("fit");
    base_card(
        r,
        shown,
        "LAB VIF · lab.diag.vif",
        "VIF_j = [Corr(X)⁻¹]_jj = n·Σ_m (R⁻¹)²_jm on the standardised factor block",
        f.n_train,
        "training rows",
    )
    .input("term", term_name(r, j + 1))
    .value(format!("{:.2}", f.vif[j]))
}

fn diag_card(r: &LabReport, shown: usize, title: &str, formula: &str) -> MathCard {
    let f = r.fit.as_ref().expect("fit");
    let c = base_card(r, shown, title, formula, f.n_train, "training residuals").input("σ", format!("{:.6e}", f.residual_sigma));
    more_caveat(c, "In-sample training residuals; descriptive, not a specification test.")
}

/// Appends to the card's caveat so the level / spurious-regression warning is never overwritten.
fn more_caveat(mut card: MathCard, text: &str) -> MathCard {
    card.caveat = Some(match card.caveat.take() {
        Some(c) => format!("{c}. {text}"),
        None => text.to_string(),
    });
    card
}

fn design_card(r: &LabReport, shown: usize) -> MathCard {
    let mut c = base_card(
        r,
        shown,
        "LAB DESIGN · lab.design.rows",
        "complete-case rows on the target's own bars; factor value at bar t−ℓ; nothing imputed or carried",
        r.design_rows,
        "complete rows",
    )
    .input("target bars", r.target_bars.to_string())
    .input("grids", r.grids.join(" | "));
    for (reason, n) in &r.dropped {
        c = c.input(format!("dropped: {reason}"), n.to_string());
    }
    c.value(r.design_rows.to_string())
}

// ---- panel (the part that paints from an immutable report) --------------------------------------

/// Min/max per pixel column for the three plotted series.
struct Envelope {
    w: usize,
    y: Vec<(f64, f64)>,
    fitted: Vec<(f64, f64)>,
    resid: Vec<(f64, f64)>,
    brk: Vec<bool>,
    /// Column index of the first purged / first test row.
    purge_at: Option<usize>,
    test_at: Option<usize>,
}

fn envelope(points: &[tr_data::studio_lab::LabPoint], w: usize) -> Envelope {
    let w = w.max(1);
    let empty = (f64::INFINITY, f64::NEG_INFINITY);
    let mut e = Envelope {
        w,
        y: vec![empty; w],
        fitted: vec![empty; w],
        resid: vec![empty; w],
        brk: vec![false; w],
        purge_at: None,
        test_at: None,
    };
    let n = points.len().max(1);
    let mut last_col = usize::MAX;
    for (i, p) in points.iter().enumerate() {
        let c = (i * w / n).min(w - 1);
        for (slot, v) in [(&mut e.y, p.y), (&mut e.fitted, p.fitted), (&mut e.resid, p.residual)] {
            slot[c].0 = slot[c].0.min(v);
            slot[c].1 = slot[c].1.max(v);
        }
        if p.break_before && c != last_col {
            e.brk[c] = true;
        }
        last_col = c;
        if p.segment == Segment::Purged && e.purge_at.is_none() {
            e.purge_at = Some(c);
        }
        if p.segment == Segment::Test && e.test_at.is_none() {
            e.test_at = Some(c);
        }
    }
    e
}

fn env_for(st: &mut LabUi, r: &LabReport, w: usize) -> Arc<Envelope> {
    if let Some((k, ww, e)) = &st.env
        && *k == r.key
        && *ww == w
    {
        return e.clone();
    }
    let e = Arc::new(envelope(&r.points, w));
    st.env = Some((r.key, w, e.clone()));
    e
}

fn env_line(p: &egui::Painter, rect: Rect, vals: &[(f64, f64)], brk: &[bool], lo: f64, hi: f64, stroke: Stroke) {
    let span = (hi - lo).max(1e-300);
    let x_of = |c: usize| rect.left() + rect.width() * (c as f32 + 0.5) / vals.len() as f32;
    let y_of = |v: f64| rect.bottom() - 2. - (rect.height() - 4.) * ((v - lo) / span) as f32;
    let mut line: Vec<Pos2> = Vec::new();
    let flush = |line: &mut Vec<Pos2>| {
        if line.len() > 1 {
            p.add(egui::Shape::line(std::mem::take(line), stroke));
        }
        line.clear();
    };
    for (c, (a, b)) in vals.iter().enumerate() {
        if !a.is_finite() {
            flush(&mut line);
            continue;
        }
        if brk[c] {
            flush(&mut line);
        }
        let (first, second) = if c % 2 == 0 { (*a, *b) } else { (*b, *a) };
        line.push(pos2(x_of(c), y_of(first)));
        if (first - second).abs() > 0. {
            line.push(pos2(x_of(c), y_of(second)));
        }
    }
    flush(&mut line);
}

fn split_regions(p: &egui::Painter, rect: Rect, e: &Envelope) {
    let x = |c: usize| rect.left() + rect.width() * c as f32 / e.w as f32;
    if let Some(t) = e.test_at {
        p.rect_filled(Rect::from_min_max(pos2(x(t), rect.top()), rect.right_bottom()), 0., theme::grid().gamma_multiply(0.18));
        p.vline(x(t), rect.y_range(), Stroke::new(1., theme::muted()));
        p.text(pos2(x(t) + 3., rect.top() + 2.), Align2::LEFT_TOP, "TEST", theme::ui_font(9.), theme::muted());
        if let Some(a) = e.purge_at {
            let (x0, x1) = (x(a), x(t).max(x(a) + 2.));
            let mut hx = x0;
            while hx < x1 {
                p.line_segment([pos2(hx, rect.bottom()), pos2((hx + 6.).min(x1), rect.top())], Stroke::new(1., theme::grid()));
                hx += 5.;
            }
            p.text(pos2(x0 - 3., rect.top() + 12.), Align2::RIGHT_TOP, "PURGE", theme::ui_font(9.), theme::muted());
        }
        p.text(pos2(rect.left() + 3., rect.top() + 2.), Align2::LEFT_TOP, "TRAIN", theme::ui_font(9.), theme::muted());
    }
}

fn plots(ui: &mut Ui, r: &LabReport, st: &mut LabUi, width: f32) {
    let Some(fit) = &r.fit else { return };
    let w = width.max(80.) as usize;
    let e = env_for(st, r, w);
    let target_unit = r.units.first().map_or("", |u| u.label());
    ui.label(ui_text(format!("FITTED vs ACTUAL · {target_unit} · ±2σ residual band (training σ)"), 10., theme::muted()));
    let (rect, _) = ui.allocate_exact_size(vec2(width, 170.), Sense::hover());
    let p = ui.painter().with_clip_rect(rect);
    p.rect_stroke(rect, 0., Stroke::new(1., theme::grid()), egui::StrokeKind::Inside);
    split_regions(&p, rect, &e);
    let s2 = 2. * fit.residual_sigma;
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for c in 0..e.w {
        for (a, b) in [e.y[c], e.fitted[c]] {
            if a.is_finite() {
                lo = lo.min(a.min(e.fitted[c].0 - s2));
                hi = hi.max(b.max(e.fitted[c].1 + s2));
            }
        }
    }
    if lo.is_finite() && hi > lo {
        // ±2σ band: neutral fill, one quad per pixel column.
        let span = hi - lo;
        let y_of = |v: f64| rect.bottom() - 2. - (rect.height() - 4.) * ((v - lo) / span) as f32;
        let mut mesh = egui::Mesh::default();
        let fill = theme::grid().gamma_multiply(0.35);
        for c in 0..e.w {
            let (a, b) = e.fitted[c];
            if !a.is_finite() {
                continue;
            }
            let (x0, x1) = (rect.left() + rect.width() * c as f32 / e.w as f32, rect.left() + rect.width() * (c + 1) as f32 / e.w as f32);
            let (yt, yb) = (y_of(b + s2), y_of(a - s2));
            let base = mesh.vertices.len() as u32;
            for pt in [pos2(x0, yt), pos2(x1, yt), pos2(x1, yb), pos2(x0, yb)] {
                mesh.vertices.push(egui::epaint::Vertex { pos: pt, uv: egui::epaint::WHITE_UV, color: fill });
            }
            mesh.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        p.add(egui::Shape::mesh(mesh));
        env_line(&p, rect, &e.y, &e.brk, lo, hi, Stroke::new(1.2, theme::series(0)));
        env_line(&p, rect, &e.fitted, &e.brk, lo, hi, Stroke::new(1.2, theme::series(1)));
    }
    p.text(rect.right_top() + vec2(-4., 2.), Align2::RIGHT_TOP, "actual · fitted", theme::ui_font(9.), theme::muted());
    ui.label(ui_text("RESIDUAL · actual − fitted", 10., theme::muted()));
    let (rect, _) = ui.allocate_exact_size(vec2(width, 90.), Sense::hover());
    let p = ui.painter().with_clip_rect(rect);
    p.rect_stroke(rect, 0., Stroke::new(1., theme::grid()), egui::StrokeKind::Inside);
    split_regions(&p, rect, &e);
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (a, b) in e.resid.iter().filter(|(a, _)| a.is_finite()) {
        lo = lo.min(*a);
        hi = hi.max(*b);
    }
    let m = lo.abs().max(hi.abs()).max(1e-300);
    if lo.is_finite() {
        let zero = rect.bottom() - 2. - (rect.height() - 4.) * 0.5;
        p.hline(rect.x_range(), zero, Stroke::new(1., theme::muted()));
        env_line(&p, rect, &e.resid, &e.brk, -m, m, Stroke::new(1., theme::text()));
    }
}

fn pct(v: f64) -> String {
    format!("{v:.0}")
}

fn table(ui: &mut Ui, r: &LabReport, st: &mut LabUi, width: f32) {
    let Some(fit) = &r.fit else { return };
    let shown = st.shown_cov.min(3);
    let inf: Option<&CoefInference> = r.inference_at(shown);
    ui.horizontal_wrapped(|ui| {
        for (i, label) in COV_CHIPS.iter().enumerate() {
            if ui.selectable_label(st.shown_cov == i, RichText::new(*label).font(theme::ui_font(11.))).clicked() {
                st.shown_cov = i;
            }
        }
    });
    match inf {
        Some(i) => {
            ui.label(mono_text(i.covariance.label(), 10., theme::muted()));
            if let Some(why) = &i.covariance.unavailable {
                ui.label(ui_text(format!("inference unavailable: {why}"), 10., theme::caution()));
            }
        }
        None => {
            ui.label(ui_text("inference withheld (LOW N)", 10., theme::caution()));
        }
    }
    ui.label(ui_text(COV_NOTES[shown], 10., theme::muted()));
    let unit = r.units.first().map_or("", |u| u.label());
    let row_h = 34.;
    let col = |f: f32| f * width;
    let max_vif = fit.vif.iter().copied().fold(10., f64::max);
    let heat = HeatScale::sequential(max_vif, "VIF", pct);
    // Header
    let (hr, _) = ui.allocate_exact_size(vec2(width, 16.), Sense::hover());
    for (x, t) in [(0.0, format!("Term · β [{unit}]")), (0.34, "SE".into()), (0.46, "95% CI".into()), (0.74, "z".into()), (0.82, "p".into()), (0.89, "q".into()), (0.95, "VIF".into())] {
        ui.painter().text(hr.left_center() + vec2(col(x), 0.), Align2::LEFT_CENTER, t, theme::ui_font(9.), theme::muted());
    }
    // Shared whisker scale across factor rows, including zero.
    let (mut lo, mut hi) = (0f64, 0f64);
    if let Some(i) = inf {
        for j in 1..fit.k {
            if let Some((a, b)) = i.interval[j] {
                lo = lo.min(a);
                hi = hi.max(b);
            }
        }
    }
    let span = (hi - lo).max(1e-300);
    for j in 0..fit.k {
        let (rect, resp) = ui.allocate_exact_size(vec2(width, row_h), Sense::hover());
        #[cfg(test)]
        if j == 1 {
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("lab-probe"), rect));
        }
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(rect, 0., theme::grid().gamma_multiply(0.2));
            p.rect_filled(Rect::from_min_size(rect.min, vec2(2., rect.height())), 0., theme::accent());
        }
        let b = fit.coefficients[j];
        p.text(rect.left_top() + vec2(4., 2.), Align2::LEFT_TOP, term_name(r, j), theme::mono(10.), theme::text());
        let y2 = rect.top() + 21.;
        let beta_txt = if j == 0 { format!("{b:+.3e}").replacen('-', "\u{2212}", 1) } else { signed(b, 4) };
        p.text(pos2(rect.left() + 4., y2), Align2::LEFT_CENTER, beta_txt, theme::mono(10.), theme::sign(b));
        let usable = inf.is_some_and(|i| i.se[j].is_some());
        let se = inf.and_then(|i| i.se[j]);
        p.text(pos2(rect.left() + col(0.34), y2), Align2::LEFT_CENTER, se.map_or("—".into(), |s| format!("{s:.2e}")), theme::mono(10.), theme::muted());
        if j > 0 {
            if let Some((a, c)) = inf.and_then(|i| i.interval[j]) {
                let (x0, x1) = (rect.left() + col(0.46), rect.left() + col(0.72));
                let xm = |v: f64| x0 + (x1 - x0) * ((v - lo) / span) as f32;
                let excl = a > 0. || c < 0.;
                let ink = if excl && usable { theme::text() } else { theme::muted().gamma_multiply(0.6) };
                p.line_segment([pos2(xm(a), y2), pos2(xm(c), y2)], Stroke::new(if excl && usable { 2. } else { 1. }, ink));
                p.vline(xm(0.), (y2 - 5.)..=(y2 + 5.), Stroke::new(1., theme::muted()));
                p.circle_stroke(pos2(xm(b), y2), 2.5, Stroke::new(1., ink));
            }
        }
        let z = inf.and_then(|i| i.z[j]);
        p.text(pos2(rect.left() + col(0.74), y2), Align2::LEFT_CENTER, z.map_or("—".into(), |z| signed(z, 2)), theme::mono(10.), z.map_or(theme::muted(), theme::sign));
        for (x, v) in [(0.82, inf.and_then(|i| i.p[j])), (0.89, inf.and_then(|i| i.q[j]))] {
            p.text(pos2(rect.left() + col(x), y2), Align2::LEFT_CENTER, v.map_or("—".into(), |v| format!("{v:.3}")), theme::mono(10.), theme::muted());
        }
        if j > 0 {
            let v = fit.vif[j - 1];
            let cell = Rect::from_min_size(pos2(rect.left() + col(0.94), rect.top() + 12.), vec2(width * 0.06 + 2., 18.));
            p.rect_filled(cell, 2., heat.fill(v));
            p.text(cell.center(), Align2::CENTER_CENTER, format!("{v:.2}"), theme::mono(9.), heat.ink(v));
            // The VIF cell has its own card; the rest of the row explains the coefficient.
            ui.interact(cell, ui.id().with(("lab-vif", j)), Sense::hover()).math_card(|| vif_card(r, shown, j - 1));
        }
        resp.math_card(|| coef_card(r, shown, j));
    }
    heat.key(ui);
}

fn kv(ui: &mut Ui, k: &str, v: String) {
    ui.label(mono_text(format!("{k} {v}"), 10., theme::muted()));
}

fn strip(ui: &mut Ui, r: &LabReport, st: &LabUi) {
    let (Some(fit), Some(h)) = (&r.fit, &r.holdout) else { return };
    let shown = st.shown_cov.min(3);
    ui.add_space(4.);
    ui.label(mono_text(
        format!(
            "TRAIN {} → {} · SPLIT {} · purged {} · n_train {} / n_test {} · split at {}% of current complete rows; recomputation with new bars moves the split",
            stamp(fit.train_start),
            stamp(fit.train_end),
            stamp(fit.split_ts),
            h.purged,
            fit.n_train,
            h.n_test,
            100 - r.spec.holdout_pct
        ),
        9.5,
        theme::muted(),
    ));
    if let Some(low) = &r.low_n {
        let resp = ui.label(ui_text(low, 11., theme::caution()));
        let rc = resp.rect;
        ui.painter().line_segment([rc.right_top(), rc.right_top() + vec2(-5., 5.)], Stroke::new(1., theme::caution()));
    }
    ui.horizontal_wrapped(|ui| {
        let (label, value, ink) = match h.r2_oos {
            Some(v) => ("R²_OOS vs training mean", signed(v, 4), theme::sign(v)),
            None => ("R²_OOS vs training mean", "—".into(), theme::muted()),
        };
        stat_tile(ui, label, &value, ink, || oos_card(r, shown));
        stat_tile(ui, "R²_in · in-sample, descriptive", &fit.r2_in.map_or("—".into(), |v| signed(v, 4)), theme::text(), || r2_in_card(r, shown));
        stat_tile(ui, "σ (training residual)", &format!("{:.3e}", fit.residual_sigma), theme::text(), || sigma_card(r, shown));
        stat_tile(ui, "κ∞ · rank", &format!("{:.2e} · {}/{}", fit.kappa_inf, fit.rank, fit.k), theme::text(), || condition_card(r, shown));
    });
    if !h.forecast_style {
        ui.label(ui_text("contemporaneous fit — not a forecast", 10., theme::muted()));
    }
    if let Some(why) = &h.unavailable {
        ui.label(ui_text(why, 10., theme::muted()));
    }
}

fn drawer(ui: &mut Ui, r: &LabReport, st: &mut LabUi, width: f32) {
    let Some(fit) = &r.fit else { return };
    let shown = st.shown_cov.min(3);
    super::studio_frame::tabs(ui, &mut st.diag_tab, &["Residual vs fitted", "Residual histogram", "Design"]);
    match st.diag_tab {
        0 | 1 => {
            let Some(d) = &r.diagnostics else { return };
            let (rect, resp) = ui.allocate_exact_size(vec2(width, 150.), Sense::hover());
            let p = ui.painter().with_clip_rect(rect);
            p.rect_stroke(rect, 0., Stroke::new(1., theme::grid()), egui::StrokeKind::Inside);
            if st.diag_tab == 0 {
                let (gx, gy) = d.grid_dims;
                let max = d.grid.iter().copied().max().unwrap_or(1).max(1) as f64;
                let heat = HeatScale::sequential(max, "count per cell", pct);
                let (cw, ch) = (rect.width() / gx as f32, rect.height() / gy as f32);
                for row in 0..gy {
                    for col in 0..gx {
                        let n = d.grid[row * gx + col];
                        if n > 0 {
                            let cell = Rect::from_min_size(
                                pos2(rect.left() + col as f32 * cw, rect.bottom() - (row + 1) as f32 * ch),
                                vec2(cw + 0.5, ch + 0.5),
                            );
                            p.rect_filled(cell, 0., heat.fill(n as f64));
                        }
                    }
                }
                p.hline(rect.x_range(), rect.center().y, Stroke::new(1., theme::muted()));
                resp.math_card(|| {
                    diag_card(r, shown, "LAB RESID/FITTED · lab.diag.resid_fitted", "48×24 counts of (ŷ, e) over training rows; y spans ±min(max|e|, 6σ)")
                        .input("ŷ range", format!("{:.4e} … {:.4e}", d.grid_x.0, d.grid_x.1))
                        .input("outside grid", d.grid_outside.to_string())
                });
                ui.label(ui_text("x: fitted ŷ · y: residual e (zero rule) · in-sample training rows", 10., theme::muted()));
                heat.key(ui);
            } else {
                let max = d.hist.iter().copied().max().unwrap_or(1).max(1) as f64;
                let heat = HeatScale::sequential(max, "count per bin", pct);
                let bw = rect.width() / d.hist.len() as f32;
                let n_total = (fit.n_train as f64).max(1.);
                let step = (d.hist_edges.1 - d.hist_edges.0) / d.hist.len() as f64;
                let top = max.max(n_total * 0.4 * step);
                let y_of = |v: f64| rect.bottom() - rect.height() * (v / top) as f32;
                let mut outline = vec![];
                for (i, c) in d.hist.iter().enumerate() {
                    let x0 = rect.left() + i as f32 * bw;
                    p.rect_filled(Rect::from_min_max(pos2(x0, y_of(*c as f64)), pos2(x0 + bw - 1., rect.bottom())), 0., heat.fill(*c as f64));
                    let mid = d.hist_edges.0 + (i as f64 + 0.5) * step;
                    let pdf = (-0.5 * mid * mid).exp() / (2. * std::f64::consts::PI).sqrt();
                    outline.push(pos2(x0 + bw / 2., y_of(n_total * pdf * step)));
                }
                p.add(egui::Shape::line(outline, Stroke::new(1., theme::text())));
                resp.math_card(|| {
                    diag_card(r, shown, "LAB RESID HIST · lab.diag.residual_hist", "e/σ in 41 bins of width 0.2 over [−4.1, 4.1]; outline = N(0,1) expected count")
                        .input("beyond range", d.hist_beyond.to_string())
                });
                ui.label(ui_text("e/σ · outline = N(0,1) expected count · in-sample training residuals", 10., theme::muted()));
                heat.key(ui);
            }
        }
        _ => {
            let resp = ui.vertical(|ui| {
                kv(ui, "n / k", format!("{} / {}", fit.n_train, fit.k));
                kv(ui, "rank", fit.rank.to_string());
                kv(ui, "κ∞", format!("{:.3e}", fit.kappa_inf));
                kv(ui, "complete rows", format!("{} of {} target bars", r.design_rows, r.target_bars));
                for (reason, n) in &r.dropped {
                    kv(ui, "dropped", format!("{n} · {reason}"));
                }
                let names: Vec<String> = std::iter::once(r.spec.target.short())
                    .chain(r.spec.factors.iter().map(|f| LabSpec::term_label(f)))
                    .collect();
                for (i, rc) in r.receipts.iter().enumerate() {
                    kv(
                        ui,
                        &names.get(i).cloned().unwrap_or_default(),
                        format!(
                            "{} · {} · digest {:016x} · gaps {} · grid {}",
                            rc.source,
                            rc.provenance.label(),
                            rc.input_digest,
                            rc.gaps.len(),
                            r.grids.get(i).map_or("—", |s| s.as_str())
                        ),
                    );
                }
            });
            resp.response.math_card(|| design_card(r, shown));
        }
    }
}

/// The report body: coefficient table, plots, holdout strip and diagnostics. Wide layouts place the
/// plots left and the table right; below 600 px everything stacks (table, tiles, then plots).
pub(crate) fn panel(ui: &mut Ui, id: PaneId, r: &LabReport, st: &mut LabUi) {
    ui.push_id(("lab-panel", id), |ui| {
        let width = ui.available_width();
        if let Some(why) = &r.unavailable {
            let (rect, _) = ui.allocate_exact_size(vec2(width, 150.), Sense::hover());
            ui.painter().rect_filled(rect, 3., theme::card());
            ui.painter().text(rect.left_top() + vec2(8., 8.), Align2::LEFT_TOP, "REGRESSION LAB UNAVAILABLE", theme::mono(11.), theme::amber());
            let g = ui.painter().layout(why.clone(), theme::ui_font(11.), theme::text(), width - 16.);
            ui.painter().galley(rect.left_top() + vec2(8., 26.), g, theme::text());
            for (s, e) in &r.factor_errors {
                ui.label(ui_text(format!("{} · {e}", tr_data::security(s).ticker), 10., theme::caution()));
            }
            if r.design_rows > 0 {
                kv(ui, "design rows", format!("{} of {} target bars", r.design_rows, r.target_bars));
            }
            for (reason, n) in &r.dropped {
                kv(ui, "dropped", format!("{n} · {reason}"));
            }
            return;
        }
        super::studio_dist::studio_heading(ui, "COEFFICIENTS");
        if width >= 600. {
            let tw = 340f32.min(width * 0.45);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(width - tw - 12.);
                    plots(ui, r, st, width - tw - 12.);
                });
                ui.vertical(|ui| {
                    ui.set_width(tw);
                    table(ui, r, st, tw);
                });
            });
            strip(ui, r, st);
        } else {
            table(ui, r, st, width);
            strip(ui, r, st);
            plots(ui, r, st, width);
        }
        super::studio_dist::studio_heading(ui, "DIAGNOSTICS");
        drawer(ui, r, st, width.min(900.));
        ui.label(ui_text(
            "selected_span does not filter Lab inputs · full-sample fit is descriptive and changes on recomputation",
            9.5,
            theme::muted(),
        ));
    });
}

#[cfg(test)]
pub(super) fn fixture_report(n: usize) -> LabReport {
    let (spec, target, rows) = tr_perf::fixtures::lab_model(n);
    fixture_compute(&spec, &target, &rows)
}

/// Dev/test fixture: the full compute on given rows (never used outside tests and the paint gate).
#[cfg(test)]
pub(super) fn fixture_compute(spec: &LabSpec, target: &NamedBars, rows: &[NamedBars]) -> LabReport {
    let close = target.bars.last().unwrap().ts + 86_400;
    lab::compute(target, rows, spec, Timeframe::D1, true, "benchmark fixture only", close, None, 7)
}

#[cfg(test)]
mod tests;
