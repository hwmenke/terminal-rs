//! Factor attribution (Studio slice 7). For a **saved frozen model**, the move over a chosen window is
//! split exactly into `actual = drift (intercept) + Σ factor contributions + residual`, in target units.
//! Nothing here fits, loads or builds a design: [`attribute`] is a pure function of the frozen fit, one
//! scored [`ResidualReport`] (which already carries the frozen design on the target grid: `y`, `fitted`,
//! `x[j]`, `segment`) and a window. Contributions are `β_j·d_j` with `d_j` the window exposure; they make
//! no causal claim. See `docs/STUDIO_ATTRIBUTION.md`.
//!
//! **Sum mode** (target Change / LogReturn, horizon `h`): consecutive design rows are overlapping h-bar
//! returns, so the window uses the stride-`h` chain `t_i = b − i·h` (`i = 0..m−1`, anchor `a = b − m·h`);
//! `A = Σ y_t` then telescopes exactly to the target's move over `(a, b]`. **Endpoint mode** (target Level /
//! LogLevel): `A = y_b − y_a` and `d = (0, x_b − x_a)`.
use crate::report_cache::ReportKey;
use crate::stable_hash::Fnv64;
use crate::studio::{SeriesReceipt, SeriesUnit, Transform};
use crate::studio_corr::relate::{MAX_POINTS, unit_for};
use crate::studio_inference::automatic_bandwidth;
use crate::studio_lab::LabSpec;
use crate::studio_residual::{FitOrigin, FrozenFit, ResidSegment, ResidualReport, valid_id};
use serde::{Deserialize, Serialize};
use tr_core::Provenance;

/// Written into every key.
pub const ATTR_METHOD: &str = "attr.v1";
pub const MAX_OBS: usize = 1_000;
pub const MIN_RHO_PAIRS: usize = crate::studio_inference::MIN_N;
/// Drift share at or above which the intercept is flagged as driving the prediction.
pub const DRIFT_LED: f64 = 0.5;

// ───────────────────────────── window and arguments ─────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttrWindow {
    /// `obs` target observations ending at `to` (`None` = latest complete row).
    Obs { obs: usize, to: Option<i64> },
    /// Anchor bar = last grid bar with `ts ≤ from`; end bar = last with `ts ≤ to` (`None` = latest complete row).
    Range { from: i64, to: Option<i64> },
}
impl Default for AttrWindow {
    fn default() -> Self {
        Self::Obs { obs: 1, to: None }
    }
}

/// `YYYY-MM-DD` (daily) or `YYYY-MM-DDTHH:MM` (UTC).
fn time_words(ts: i64, daily: bool) -> String {
    let d = chrono::DateTime::from_timestamp(ts, 0).unwrap_or_default();
    // A bare date parses as 23:59:59, so it round-trips only for exactly that instant.
    let _ = daily;
    let bare = ts.rem_euclid(86400) == 86399;
    d.format(if bare { "%Y-%m-%d" } else { "%Y-%m-%dT%H:%M" }).to_string()
}
/// UTC stamp used in reasons and receipts.
pub fn stamp(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).map_or_else(|| "—".into(), |d| d.format("%Y-%m-%d %H:%M UTC").to_string())
}

impl AttrWindow {
    /// Canonical, re-parseable words (layout persistence and the Copy command).
    pub fn words(&self, daily: bool) -> String {
        match self {
            Self::Obs { obs, to: None } => format!("obs={obs}"),
            Self::Obs { obs, to: Some(t) } => format!("obs={obs} to={}", time_words(*t, daily)),
            Self::Range { from, to: None } => format!("from={}", time_words(*from, daily)),
            Self::Range { from, to: Some(t) } => {
                format!("from={} to={}", time_words(*from, daily), time_words(*t, daily))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AttrArgs {
    pub model: Option<String>,
    pub window: Option<AttrWindow>,
}

fn parse_time(s: &str) -> Result<i64, String> {
    use chrono::{NaiveDate, NaiveDateTime};
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(d.and_hms_opt(23, 59, 59).expect("valid time").and_utc().timestamp());
    }
    let t = s.strip_suffix('Z').unwrap_or(s);
    NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M")
        .map(|d| d.and_utc().timestamp())
        .map_err(|_| format!("`{s}` is not a UTC time like 2026-03-02 or 2026-03-02T14:30"))
}

impl AttrArgs {
    /// Words after `STUDIO attr`: `[model=<id>] [obs=N | from=<t>] [to=<t>]`, each key at most once.
    pub fn parse(words: &str) -> Result<Self, String> {
        let (mut model, mut obs, mut from, mut to) = (None, None, None, None);
        for word in words.split_whitespace() {
            let Some((key, value)) = word.split_once('=') else {
                return Err(format!("unknown ATTR input `{word}`; use model=, obs=, from= or to="));
            };
            let value = value.trim_matches('"');
            match key.to_ascii_lowercase().as_str() {
                "model" if model.is_none() => {
                    if !valid_id(value) {
                        return Err(format!("`{value}` is not a model id like AAPL-v-SPY-v1"));
                    }
                    model = Some(value.to_string());
                }
                "obs" if obs.is_none() => {
                    obs = Some(
                        value
                            .parse::<usize>()
                            .ok()
                            .filter(|n| (1..=MAX_OBS).contains(n))
                            .ok_or(format!("obs must be 1–{MAX_OBS}"))?,
                    );
                }
                "from" if from.is_none() => from = Some(parse_time(value)?),
                "to" if to.is_none() => to = Some(parse_time(value)?),
                _ => return Err(format!("unknown or repeated ATTR input `{word}`")),
            }
        }
        if obs.is_some() && from.is_some() {
            return Err("obs= and from= are mutually exclusive".into());
        }
        if let (Some(f), Some(t)) = (from, to)
            && f > t
        {
            return Err("from= must not be after to=".into());
        }
        let window = match (obs, from, to) {
            (_, Some(from), to) => Some(AttrWindow::Range { from, to }),
            (None, None, None) => None,
            (obs, None, to) => Some(AttrWindow::Obs { obs: obs.unwrap_or(1), to }),
        };
        Ok(Self { model, window })
    }
}

// ───────────────────────────── report types ─────────────────────────────

/// Which uncertainty the display shows. A UI choice only: never keyed, never recomputed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AttributionSe {
    Coefficient,
    #[default]
    Predictive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttrMode {
    Sum { h: usize },
    Endpoint,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExposureKind {
    /// Telescopes to the factor's own move over `(a−ℓ, b−ℓ]` (target-grid bars).
    Move { from_ts: i64, to_ts: i64 },
    EndpointMove,
    /// Not one move: the contribution is exact but the "move" column is N/A with `why`.
    NotAMove { why: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactorContribution {
    pub factor: String,
    pub label: String,
    pub transform: Transform,
    pub lag: usize,
    pub unit: SeriesUnit,
    /// `d_j`: Σ over the chain rows, or `x_b − x_a`.
    pub exposure: f64,
    pub kind: ExposureKind,
    pub beta: f64,
    /// `β_j·d_j`, target units.
    pub contribution: f64,
    /// `|d_j|·√V_jj` per stored covariance entry.
    pub coefficient_se: Vec<Option<f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CovarianceSe {
    pub label: String,
    /// `|d_0|·√V_00`.
    pub intercept_se: Option<f64>,
    /// `√(dᵀVd)`.
    pub coefficient_se: Option<f64>,
    /// `√(dᵀVd + Var_W)`.
    pub predictive_se: Option<f64>,
    pub unavailable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResidualVariance {
    /// `σ²_saved` (df-corrected, per h-bar observation).
    pub sigma2: f64,
    /// `Var_W`: the residual term of the window.
    pub variance: Option<f64>,
    /// `(lag in rows, ρ̂, pairs)`.
    pub rho: Vec<(usize, f64, usize)>,
    /// Bartlett truncation `L` used (sum mode).
    pub truncation: usize,
    /// Tapered sum ≤ 0 → `m·σ²`.
    pub fallback: bool,
    pub unavailable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WindowLabel {
    InSample,
    OutOfSample,
    Mixed { train: usize, embargo: usize, scored: usize },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttrPathPoint {
    pub ts: i64,
    pub actual: f64,
    pub drift: f64,
    pub factors: Vec<f64>,
    pub residual: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttributionReport {
    pub key: u64,
    pub score_key: u64,
    pub model_digest: u64,
    pub model_id: String,
    pub window: AttrWindow,
    pub mode: AttrMode,
    pub from_ts: i64,
    pub to_ts: i64,
    pub m: usize,
    pub actual: f64,
    pub intercept: f64,
    pub factors: Vec<FactorContribution>,
    pub predicted: f64,
    pub residual: f64,
    /// `Σ e_t` (sum mode) or `e_b − e_a` (endpoint), from the scored report: the cross-check.
    pub residual_scored_sum: f64,
    /// `expm1(A)` for log targets; shown, never decomposed.
    pub simple_actual: Option<f64>,
    pub unit: SeriesUnit,
    pub display_scale: f64,
    pub display_unit: &'static str,
    pub covariances: Vec<CovarianceSe>,
    pub default_cov: Option<usize>,
    pub residual_var: ResidualVariance,
    pub label: WindowLabel,
    pub trained_thru: i64,
    pub embargo_bars: usize,
    pub drift_share: Option<f64>,
    pub drift_led: bool,
    pub caveats: Vec<String>,
    pub path: Vec<AttrPathPoint>,
    pub receipts: Vec<SeriesReceipt>,
    pub source: String,
    pub provenance: Provenance,
    pub closed_thru: i64,
    pub observed_at: Option<i64>,
    pub unavailable: Option<String>,
}

// ───────────────────────────── pure maths ─────────────────────────────

/// `c = (β₀d₀, β₁d₁, …)`, `Â = Σ c`, `R = A − Â`.
pub fn decompose(beta: &[f64], d: &[f64], actual: f64) -> (Vec<f64>, f64, f64) {
    let c: Vec<f64> = beta.iter().zip(d).map(|(b, x)| b * x).collect();
    let predicted: f64 = c.iter().sum();
    (c, predicted, actual - predicted)
}

/// `dᵀVd`, every cross-term included. `None` when non-finite or negative beyond rounding.
pub fn quad_form(d: &[f64], v: &[Vec<f64>]) -> Option<f64> {
    if v.len() != d.len() || v.iter().any(|r| r.len() != d.len()) {
        return None;
    }
    let (mut q, mut scale) = (0., 0.);
    for (i, a) in d.iter().enumerate() {
        for (j, b) in d.iter().enumerate() {
            let t = a * b * v[i][j];
            q += t;
            scale += t.abs();
        }
    }
    if !q.is_finite() || q < -1e-12 * scale {
        return None;
    }
    Some(q.max(0.))
}

/// Sample autocorrelation of stored residuals at `lag`: `c(ℓ)/c(0)` with `c(ℓ) = (1/N)Σ(e_t−ē)(e_{t−ℓ}−ē)`
/// over pairs inside one run (no `breaks[s]` for `s ∈ (t−ℓ, t]`). Returns the pair count; no pairs → `None`.
pub fn rho_hat(e: &[f64], breaks: &[bool], lag: usize) -> Option<(f64, usize)> {
    let n = e.len();
    if n == 0 || lag == 0 || lag >= n || breaks.len() != n {
        return None;
    }
    let mean = e.iter().sum::<f64>() / n as f64;
    let c0 = e.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64;
    if !(c0 > 0.) {
        return None;
    }
    // prefix[i] = breaks in [0, i)
    let mut prefix = Vec::with_capacity(n + 1);
    prefix.push(0usize);
    for b in breaks {
        prefix.push(prefix.last().copied().unwrap_or(0) + usize::from(*b));
    }
    let (mut acc, mut pairs) = (0., 0usize);
    for t in lag..n {
        // breaks at s ∈ (t−lag, t]  ==  indices t−lag+1 ..= t
        if prefix[t + 1] - prefix[t - lag + 1] == 0 {
            acc += (e[t] - mean) * (e[t - lag] - mean);
            pairs += 1;
        }
    }
    (pairs > 0).then(|| (acc / n as f64 / c0, pairs))
}

/// Bartlett-tapered variance of a sum of `m` stride-`h` residuals: `σ²[m + 2Σ(m−s)k_s ρ̂(sh)]`,
/// `rho_stride[s−1] = ρ̂(s·h)`, `L = rho_stride.len()`, `k_s = 1 − s/(L+1)`. A bracket ≤ 0 gives `m·σ²`
/// and `true`.
pub fn residual_variance_sum(m: usize, sigma2: f64, rho_stride: &[f64]) -> (f64, bool) {
    let l = rho_stride.len();
    let mut bracket = m as f64;
    for (i, rho) in rho_stride.iter().enumerate() {
        let s = i + 1;
        if s >= m {
            break;
        }
        let k = 1. - s as f64 / (l + 1) as f64;
        bracket += 2. * (m - s) as f64 * k * rho;
    }
    if bracket > 0. { (sigma2 * bracket, false) } else { (sigma2 * m as f64, true) }
}

/// Endpoint difference of two residuals `ℓ` rows apart: `2σ²(1 − ρ̂(ℓ))`.
pub fn residual_variance_endpoint(sigma2: f64, rho: f64) -> f64 {
    2. * sigma2 * (1. - rho)
}

// ───────────────────────────── window resolution ─────────────────────────────

struct Resolved {
    a: usize,
    b: usize,
    m: usize,
    /// Rows used, newest first (sum mode), or `[b, a]` (endpoint).
    rows: Vec<usize>,
    latest_incomplete: bool,
}

fn at_or_before(ts: &[i64], bound: i64) -> Option<usize> {
    ts.partition_point(|t| *t <= bound).checked_sub(1)
}

fn resolve(r: &ResidualReport, mode: AttrMode, w: &AttrWindow) -> Result<Resolved, String> {
    let ts = &r.residual.ts;
    let vals = &r.residual.values;
    let Some(first) = ts.first() else { return Err("the scored grid is empty".into()) };
    let before = |what: &str, bound: i64| {
        format!(
            "window {what} {} is before the scored grid (first bar {}); the report keeps the latest {MAX_POINTS} bars",
            stamp(bound),
            stamp(*first)
        )
    };
    let (to_bound, obs, from_bound) = match w {
        AttrWindow::Obs { obs, to } => (*to, Some(*obs), None),
        AttrWindow::Range { from, to } => (*to, None, Some(*from)),
    };
    let (b, latest_incomplete) = match to_bound {
        Some(t) => (at_or_before(ts, t).ok_or_else(|| before("end", t))?, false),
        None => {
            let i = vals.iter().rposition(Option::is_some).ok_or("no complete model row in the scored grid")?;
            (i, i + 1 != ts.len())
        }
    };
    let step = match mode {
        AttrMode::Sum { h } => h,
        AttrMode::Endpoint => 1,
    };
    let (a, m) = match (obs, from_bound) {
        (Some(obs), _) => {
            let span = obs.checked_mul(step).filter(|s| *s <= b);
            let Some(span) = span else {
                return Err(format!(
                    "window starts before the scored grid (first bar {}); the report keeps the latest {MAX_POINTS} bars",
                    stamp(*first)
                ));
            };
            (b - span, obs)
        }
        (None, Some(from)) => {
            let a = at_or_before(ts, from).ok_or_else(|| before("start", from))?;
            if b <= a {
                return Err("window is empty: the end bar is not after the anchor bar".into());
            }
            let len = b - a;
            match mode {
                AttrMode::Sum { h } if len % h != 0 => {
                    return Err(format!(
                        "window length {len} bars is not a multiple of h={h}; overlapping h-bar returns cannot be summed (try obs={})",
                        len / h
                    ));
                }
                AttrMode::Sum { h } => (a, len / h),
                AttrMode::Endpoint => (a, len),
            }
        }
        (None, None) => unreachable!("a window is either Obs or Range"),
    };
    let incomplete = |t: usize| format!("window incomplete: no complete model row at {}", stamp(ts[t]));
    let rows: Vec<usize> = match mode {
        AttrMode::Sum { h } => {
            let rows: Vec<usize> = (0..m).map(|i| b - i * h).collect();
            if let Some(t) = rows.iter().find(|t| vals[**t].is_none()) {
                return Err(incomplete(*t));
            }
            for (i, t) in rows.iter().enumerate() {
                if i + 1 < m && r.residual.break_before[*t] {
                    return Err(format!("window crosses a data break before {}", stamp(ts[*t])));
                }
            }
            rows
        }
        AttrMode::Endpoint => {
            for t in [b, a] {
                if vals[t].is_none() {
                    return Err(incomplete(t));
                }
            }
            vec![b, a]
        }
    };
    Ok(Resolved { a, b, m, rows, latest_incomplete })
}

// ───────────────────────────── attribute ─────────────────────────────

fn display_for(t: Transform) -> (f64, &'static str) {
    match t {
        Transform::LogReturn { .. } | Transform::LogLevel => (1e4, "log bp"),
        Transform::Change { .. } => (1., "Δ price"),
        Transform::Level => (1., "price"),
    }
}

/// Identity of an attribution: the scored report's key (every input bar digest, gap, source, closed
/// boundary and the model digest), the model, the window and the method. Never a wall clock.
pub fn attribution_key(fit: &FrozenFit, r: &ResidualReport, w: &AttrWindow) -> ReportKey {
    let mut h = Fnv64::default();
    h.write_u64(r.key);
    h.write_u64(fit.content_digest);
    let opt = |h: &mut Fnv64, v: Option<i64>| match v {
        Some(t) => {
            h.write_u64(1);
            h.write_i64(t);
        }
        None => h.write_u64(0),
    };
    match w {
        AttrWindow::Obs { obs, to } => {
            h.write_u64(1);
            h.write_u64(*obs as u64);
            opt(&mut h, *to);
        }
        AttrWindow::Range { from, to } => {
            h.write_u64(2);
            h.write_i64(*from);
            opt(&mut h, *to);
        }
    }
    h.write_str(ATTR_METHOD);
    h.write_u64(MAX_POINTS as u64);
    h.write_u64(fit.coef_covariance.len() as u64);
    let mut key = ReportKey::new("studio_attr_v1", &fit.model.symbol, fit.model.tf, &r.source, &[], ATTR_METHOD);
    key.params = h.finish();
    key
}

fn blank(fit: &FrozenFit, r: &ResidualReport, w: &AttrWindow, mode: AttrMode, reason: String) -> AttributionReport {
    let (display_scale, display_unit) = display_for(fit.model.target);
    AttributionReport {
        key: attribution_key(fit, r, w).params,
        score_key: r.key,
        model_digest: fit.content_digest,
        model_id: fit.id.clone(),
        window: w.clone(),
        mode,
        from_ts: 0,
        to_ts: 0,
        m: 0,
        actual: 0.,
        intercept: 0.,
        factors: vec![],
        predicted: 0.,
        residual: 0.,
        residual_scored_sum: 0.,
        simple_actual: None,
        unit: unit_for(fit.model.target),
        display_scale,
        display_unit,
        covariances: vec![],
        default_cov: None,
        residual_var: ResidualVariance {
            sigma2: fit.residual_sigma * fit.residual_sigma,
            variance: None,
            rho: vec![],
            truncation: 0,
            fallback: false,
            unavailable: None,
        },
        label: WindowLabel::OutOfSample,
        trained_thru: fit.trained_thru,
        embargo_bars: fit.embargo_bars,
        drift_share: None,
        drift_led: false,
        caveats: vec![],
        path: vec![],
        receipts: r.receipts.clone(),
        source: r.source.clone(),
        provenance: r.provenance,
        closed_thru: r.closed_thru,
        observed_at: r.observed_at,
        unavailable: Some(reason),
    }
}

fn factor_kind(
    mode: AttrMode,
    f: &crate::studio_lab::FactorSpec,
    r: &ResidualReport,
    a: usize,
    b: usize,
) -> ExposureKind {
    let ret_h = match f.transform {
        Transform::Change { h } | Transform::LogReturn { h } => Some(h),
        Transform::Level | Transform::LogLevel => None,
    };
    match (mode, ret_h) {
        (AttrMode::Sum { h }, Some(hj)) if hj == h => match (a.checked_sub(f.lag), b.checked_sub(f.lag)) {
            (Some(from), Some(to)) => ExposureKind::Move { from_ts: r.residual.ts[from], to_ts: r.residual.ts[to] },
            _ => ExposureKind::NotAMove { why: "lagged window anchor precedes the scored grid".into() },
        },
        (AttrMode::Sum { .. }, Some(hj)) => ExposureKind::NotAMove {
            why: format!("Σ of m overlapping/strided {hj}-bar values; h_j ≠ h_y"),
        },
        (AttrMode::Sum { .. }, None) => {
            ExposureKind::NotAMove { why: "Σ of m levels = m × mean level; not a move".into() }
        }
        (AttrMode::Endpoint, None) => ExposureKind::EndpointMove,
        (AttrMode::Endpoint, Some(hj)) => {
            ExposureKind::NotAMove { why: format!("difference of two {hj}-bar values; not a move") }
        }
    }
}

/// Pure attribution of the frozen `fit` over `w`, from the scored report `r`. Never fits, loads or rebuilds
/// a design. An unusable window or report gives `unavailable` with the exact reason and no numbers.
pub fn attribute(fit: &FrozenFit, r: &ResidualReport, w: &AttrWindow) -> AttributionReport {
    let k = fit.model.k;
    let mode = match fit.model.target {
        Transform::Change { h } | Transform::LogReturn { h } => AttrMode::Sum { h: h.max(1) },
        Transform::Level | Transform::LogLevel => AttrMode::Endpoint,
    };
    let fail = |why: String| blank(fit, r, w, mode, why);
    if let Some(u) = &r.unavailable {
        return fail(format!("the scored model is unavailable: {u}"));
    }
    if r.model_digest != fit.content_digest {
        return fail("the scored report belongs to a different model".into());
    }
    if let Err(e) = r.provenance.require_verified("Attribution") {
        return fail(e);
    }
    let n = r.residual.ts.len();
    if fit.model.coefficients.len() != k
        || r.x.len() != k - 1
        || r.y.len() != n
        || r.fitted.len() != n
        || r.segment.len() != n
        || r.residual.values.len() != n
        || r.residual.break_before.len() != n
        || r.x.iter().any(|c| c.len() != n)
    {
        return fail("the scored report does not carry the frozen design for this model".into());
    }
    let res = match resolve(r, mode, w) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    let Resolved { a, b, m, rows, latest_incomplete } = res;
    let ts = &r.residual.ts;
    let beta = &fit.model.coefficients;
    let get = |col: &Vec<Option<f64>>, t: usize| col[t].ok_or_else(|| format!("window incomplete: no complete model row at {}", stamp(ts[t])));
    // Exposure vector d and actual A.
    let mut d = vec![0.; k];
    let mut actual = 0.;
    let built: Result<(), String> = (|| {
        match mode {
            AttrMode::Sum { .. } => {
                d[0] = m as f64;
                for t in &rows {
                    actual += get(&r.y, *t)?;
                    for j in 1..k {
                        d[j] += get(&r.x[j - 1], *t)?;
                    }
                }
            }
            AttrMode::Endpoint => {
                actual = get(&r.y, b)? - get(&r.y, a)?;
                for j in 1..k {
                    d[j] = get(&r.x[j - 1], b)? - get(&r.x[j - 1], a)?;
                }
            }
        }
        Ok(())
    })();
    if let Err(e) = built {
        return fail(e);
    }
    let (c, predicted, residual) = decompose(beta, &d, actual);
    // Cross-checks against the scored report.
    let resid_at = |t: usize| r.residual.values[t].unwrap_or(f64::NAN);
    let residual_scored_sum = match mode {
        AttrMode::Sum { .. } => rows.iter().map(|t| resid_at(*t)).sum::<f64>(),
        AttrMode::Endpoint => resid_at(b) - resid_at(a),
    };
    let tol_scale = actual.abs()
        + c.iter().map(|v| v.abs()).sum::<f64>()
        + match mode {
            AttrMode::Sum { .. } => rows.iter().map(|t| resid_at(*t).abs()).sum::<f64>(),
            AttrMode::Endpoint => resid_at(a).abs() + resid_at(b).abs(),
        };
    debug_assert!(
        (residual - residual_scored_sum).abs() <= 1e-12 * tol_scale,
        "attribution residual {residual} disagrees with the scored residuals {residual_scored_sum}"
    );

    // Factor rows with kinds and coefficient SEs.
    let covs = &fit.coef_covariance;
    let mut factors = Vec::with_capacity(k - 1);
    for (j, f) in fit.model.factors.iter().enumerate() {
        let se = covs
            .iter()
            .map(|cv| cv.covariance.as_ref().map(|v| d[j + 1].abs() * v[j + 1][j + 1].max(0.).sqrt()))
            .collect();
        factors.push(FactorContribution {
            factor: f.symbol.clone(),
            label: LabSpec::term_label(f),
            transform: f.transform,
            lag: f.lag,
            unit: unit_for(f.transform),
            exposure: d[j + 1],
            kind: factor_kind(mode, f, r, a, b),
            beta: beta[j + 1],
            contribution: c[j + 1],
            coefficient_se: se,
        });
    }

    // Residual variance of the window from the stored training residuals.
    let sigma2 = fit.residual_sigma * fit.residual_sigma;
    let mut rv = ResidualVariance { sigma2, variance: None, rho: vec![], truncation: 0, fallback: false, unavailable: None };
    match mode {
        AttrMode::Sum { h } => {
            let wanted = (m - 1).min(automatic_bandwidth(fit.train_residuals.len()));
            for s in 1..=wanted {
                match rho_hat(&fit.train_residuals, &fit.train_breaks, s * h) {
                    Some((rho, pairs)) if pairs >= MIN_RHO_PAIRS => rv.rho.push((s * h, rho, pairs)),
                    _ => break,
                }
            }
            rv.truncation = rv.rho.len();
            let stride: Vec<f64> = rv.rho.iter().map(|x| x.1).collect();
            let (v, fallback) = residual_variance_sum(m, sigma2, &stride);
            rv.variance = Some(v);
            rv.fallback = fallback;
        }
        AttrMode::Endpoint => match rho_hat(&fit.train_residuals, &fit.train_breaks, b - a) {
            Some((rho, pairs)) if pairs >= MIN_RHO_PAIRS => {
                rv.rho.push((b - a, rho, pairs));
                rv.variance = Some(residual_variance_endpoint(sigma2, rho));
            }
            other => {
                let pairs = other.map_or(0, |x| x.1);
                rv.unavailable = Some(format!(
                    "LOW N: {pairs} stored residual pairs at lag {} < {MIN_RHO_PAIRS}; predictive SE withheld",
                    b - a
                ));
            }
        },
    }
    // Per-covariance totals.
    let covariances: Vec<CovarianceSe> = covs
        .iter()
        .map(|cv| {
            let mut out = CovarianceSe {
                label: cv.label(),
                intercept_se: None,
                coefficient_se: None,
                predictive_se: None,
                unavailable: cv.unavailable.clone(),
            };
            if let Some(v) = &cv.covariance {
                out.intercept_se = Some(d[0].abs() * v[0][0].max(0.).sqrt());
                out.coefficient_se = quad_form(&d, v).map(f64::sqrt);
                out.predictive_se = match (quad_form(&d, v), rv.variance) {
                    (Some(q), Some(w)) => Some((q + w).sqrt()),
                    _ => None,
                };
                if out.coefficient_se.is_none() {
                    out.unavailable = Some("coefficient variance is not finite and non-negative".into());
                }
            } else if out.unavailable.is_none() {
                out.unavailable = Some("no covariance matrix stored".into());
            }
            out
        })
        .collect();
    let default_cov = match fit.model.origin {
        FitOrigin::Lab => covs
            .get(3)
            .filter(|c| c.covariance.is_some())
            .map(|_| 3)
            .or_else(|| covs.iter().position(|c| c.covariance.is_some())),
        FitOrigin::Relate => covs.iter().position(|c| c.covariance.is_some()),
    };

    // Label from the segments of the rows used.
    let (mut train, mut embargo, mut scored) = (0, 0, 0);
    for t in &rows {
        match r.segment[*t] {
            ResidSegment::Train => train += 1,
            ResidSegment::Embargo => embargo += 1,
            ResidSegment::Scored => scored += 1,
        }
    }
    let label = if embargo == 0 && scored == 0 {
        WindowLabel::InSample
    } else if train == 0 && embargo == 0 {
        WindowLabel::OutOfSample
    } else {
        WindowLabel::Mixed { train, embargo, scored }
    };

    // Drift share (sum mode only) and its flag.
    let drift_share = match mode {
        AttrMode::Sum { .. } => {
            let rest: f64 = c[1..].iter().map(|x| x.abs()).sum();
            let tot = c[0].abs() + rest;
            (tot > 0.).then(|| c[0].abs() / tot)
        }
        AttrMode::Endpoint => None,
    };
    let drift_led = drift_share.is_some_and(|s| s >= DRIFT_LED);

    // Caveats.
    let mut caveats = Vec::new();
    if latest_incomplete {
        let last = ts.last().copied().unwrap_or(0);
        caveats.push(format!("latest bar {} has no complete model row", stamp(last)));
    }
    if fit.model.tf == tr_core::Timeframe::D1 {
        let mut cash = std::iter::once(&fit.model.symbol)
            .chain(fit.model.factors.iter().map(|f| &f.symbol))
            .map(|s| crate::time_link::cash_class(crate::security(s).class));
        if let Some(first) = cash.next()
            && cash.any(|c| c != first)
        {
            caveats.push("daily closes are not synchronous across NYSE cash and UTC-grid inputs".into());
        }
    }

    // Path through time.
    let mut path = Vec::new();
    match mode {
        AttrMode::Sum { .. } => {
            let mut p = AttrPathPoint { ts: 0, actual: 0., drift: 0., factors: vec![0.; k - 1], residual: 0. };
            for t in rows.iter().rev() {
                p.ts = ts[*t];
                p.actual += r.y[*t].unwrap_or(0.);
                p.drift += beta[0];
                for j in 1..k {
                    p.factors[j - 1] += beta[j] * r.x[j - 1][*t].unwrap_or(0.);
                }
                p.residual += resid_at(*t);
                path.push(p.clone());
            }
        }
        AttrMode::Endpoint => {
            let (ya, ea) = (r.y[a].unwrap_or(0.), resid_at(a));
            for t in a + 1..=b {
                let (Some(y), Some(_)) = (r.y[t], r.residual.values[t]) else { continue };
                path.push(AttrPathPoint {
                    ts: ts[t],
                    actual: y - ya,
                    drift: 0.,
                    factors: (1..k)
                        .map(|j| beta[j] * (r.x[j - 1][t].unwrap_or(f64::NAN) - r.x[j - 1][a].unwrap_or(f64::NAN)))
                        .collect(),
                    residual: resid_at(t) - ea,
                });
            }
        }
    }

    let (display_scale, display_unit) = display_for(fit.model.target);
    AttributionReport {
        key: attribution_key(fit, r, w).params,
        score_key: r.key,
        model_digest: fit.content_digest,
        model_id: fit.id.clone(),
        window: w.clone(),
        mode,
        from_ts: ts[a],
        to_ts: ts[b],
        m,
        actual,
        intercept: c[0],
        factors,
        predicted,
        residual,
        residual_scored_sum,
        simple_actual: matches!(fit.model.target, Transform::LogReturn { .. } | Transform::LogLevel)
            .then(|| actual.exp_m1()),
        unit: unit_for(fit.model.target),
        display_scale,
        display_unit,
        covariances,
        default_cov,
        residual_var: rv,
        label,
        trained_thru: fit.trained_thru,
        embargo_bars: fit.embargo_bars,
        drift_share,
        drift_led,
        caveats,
        path,
        receipts: r.receipts.clone(),
        source: r.source.clone(),
        provenance: r.provenance,
        closed_thru: r.closed_thru,
        observed_at: r.observed_at,
        unavailable: None,
    }
}

impl AttributionReport {
    /// `Σ_{j≥1} c_j`.
    pub fn factor_total(&self) -> f64 {
        self.factors.iter().map(|f| f.contribution).sum()
    }
    /// The covariance entry the display uses (`default_cov` unless the user picked another).
    pub fn cov(&self, pick: Option<usize>) -> Option<&CovarianceSe> {
        pick.filter(|i| *i < self.covariances.len()).or(self.default_cov).and_then(|i| self.covariances.get(i))
    }
}

#[cfg(test)]
#[path = "studio_attribution_tests.rs"]
mod tests;
