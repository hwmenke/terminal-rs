//! Relationship Studio: verified, timestamp-aligned log returns and classical OLS.
//! No fill-forward. Returns span adjacent common price endpoints, excluding vault joins.
use crate::studio_inference::{self as inference, CovarianceMethod, RegressionCovariance};
use serde::{Deserialize, Serialize};
use tr_core::{Bar, Provenance};

fn one_horizon() -> usize {
    1
}

pub const DEFAULT_SET: [&str; 7] = ["SPY", "QQQ", "IWM", "TLT", "GLD", "USO", "UUP"];
pub const ROLLING_WINDOWS: [usize; 3] = [20, 60, 120];

#[derive(Debug, Clone, Default)]
pub struct NamedBars {
    pub symbol: String,
    pub bars: Vec<Bar>,
    pub source: String,
    pub error: Option<String>,
    /// Vault UNVERIFIED JOIN gaps `(last before, first after)`; no return spans one.
    pub gaps: Vec<(i64, i64)>,
    /// Structured source receipt (display/Inspector/provider identity); not part of any cache key.
    pub receipt: Option<crate::source_receipt::SourceReceipt>,
}

/// How bars are joined and annualised.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    pub annual_periods: f64,
    /// Daily bars join on the UTC calendar date (sources may stamp the same
    /// session at different times of day); intraday bars join on exact timestamps.
    pub daily: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairReturn {
    pub start: i64,
    pub end: i64,
    pub asset: f64,
    pub benchmark: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ols {
    pub n: usize,
    pub alpha: Option<f64>,
    pub alpha_annual: Option<f64>,
    pub beta: Option<f64>,
    pub r_squared: Option<f64>,
    pub correlation: Option<f64>,
    pub alpha_se: Option<f64>,
    pub beta_se: Option<f64>,
    pub alpha_t: Option<f64>,
    pub beta_t: Option<f64>,
    /// Regression standard error, sqrt(SSE / (n - 2)), per observation.
    pub residual_sigma: Option<f64>,
    pub residual_vol_annual: Option<f64>,
    /// Mean asset log return / mean benchmark log return on benchmark up/down bars.
    pub up_capture: Option<f64>,
    pub down_capture: Option<f64>,
    pub upside_beta: Option<f64>,
    pub downside_beta: Option<f64>,
    pub up_n: usize,
    pub down_n: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rolling {
    pub window: usize,
    pub points: Vec<(i64, Option<f64>, Option<f64>)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathPoint {
    pub ts: i64,
    pub log_ratio: f64,
    pub z60: Option<f64>,
    pub residual_return: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatrixCell {
    pub correlation: Option<f64>,
    pub n: usize,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Matrix {
    pub symbols: Vec<String>,
    pub cells: Vec<Vec<MatrixCell>>,
    pub unavailable: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FitInference {
    pub covariance: RegressionCovariance,
    /// Two fixed tests: alpha=0, beta=1. Never selected from observed results.
    pub p: [Option<f64>; 2],
    pub q: [f64; 2],
    pub intervals: [Option<(f64, f64)>; 2],
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BetaBandPoint {
    pub ts: i64,
    pub start: i64,
    pub beta: Option<f64>,
    pub interval: Option<(f64, f64)>,
    /// Starts a new polygon run, including the first available point after a join.
    pub break_before: bool,
    pub covariance_label: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Holdout {
    /// Training pairs actually fitted (after the purge).
    pub train_n: usize,
    pub test_n: usize,
    /// Nominal training pairs dropped because their h-bar support overlaps the first test pair
    /// (h−1; 0 at h=1). Older saved reports restore 0.
    #[serde(default)]
    pub purged: usize,
    pub split_ts: i64,
    pub alpha: Option<f64>,
    pub beta: Option<f64>,
    pub train_mean: Option<f64>,
    pub r_squared_oos: Option<f64>,
    pub unavailable: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RobustReport {
    pub fits: Vec<FitInference>,
    /// 60-observation pointwise bands; at most 1,000 trailing endpoints per SE.
    pub rolling: Vec<Vec<BetaBandPoint>>,
    pub holdout: Option<Holdout>,
    #[serde(default)]
    pub theil_sen: Option<TheilSen>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TheilSen {
    pub n: usize,
    pub slopes: usize,
    pub slope: Option<f64>,
}
/// Descriptive robust cross-check, no p-value; at most the latest 2,000 pairs.
pub fn theil_sen(pairs: &[PairReturn]) -> TheilSen {
    let p = &pairs[pairs.len().saturating_sub(2000)..];
    let mut slopes = Vec::with_capacity(p.len().saturating_mul(p.len().saturating_sub(1)) / 2);
    for (i, a) in p.iter().enumerate() {
        for b in &p[i + 1..] {
            if let Some(v) = divide(b.asset - a.asset, b.benchmark - a.benchmark) {
                slopes.push(v);
            }
        }
    }
    let count = slopes.len();
    let slope = if count == 0 {
        None
    } else {
        let mid = count / 2;
        slopes.select_nth_unstable_by(mid, f64::total_cmp);
        Some(if count % 2 == 0 {
            0.5 * slopes[mid] + 0.5 * slopes[..mid].iter().copied().max_by(f64::total_cmp).unwrap()
        } else {
            slopes[mid]
        })
    };
    TheilSen { n: p.len(), slopes: count, slope }
}

pub const SE_METHODS: [CovarianceMethod; 4] =
    [CovarianceMethod::Classical, CovarianceMethod::HC3, CovarianceMethod::HacFixed5, CovarianceMethod::HacAutomatic];

fn fit_inference(pairs: &[PairReturn], ols: &Ols, breaks: &[bool], method: CovarianceMethod) -> FitInference {
    let design: Vec<_> = pairs.iter().map(|p| vec![1., p.benchmark]).collect();
    let residual: Vec<_> =
        pairs.iter().map(|p| p.asset - ols.alpha.unwrap_or(0.) - ols.beta.unwrap_or(0.) * p.benchmark).collect();
    let mut covariance = inference::regression_covariance(&design, &residual, breaks, method);
    // Exact fits can have a few ulps of residual noise. Do not manufacture tests.
    let magnitude = pairs.iter().map(|p| p.asset.abs()).fold(0., f64::max);
    if ols.alpha.is_none()
        || ols.beta.is_none()
        || ols.residual_sigma.is_none_or(|s| s <= 64. * f64::EPSILON * magnitude)
    {
        covariance.covariance = None;
        covariance.unavailable = Some("singular design or numerically zero residual variance".into());
    }
    if covariance.covariance.as_ref().is_some_and(|v| v[0][0] <= 0. || v[1][1] <= 0.) {
        covariance.covariance = None;
        covariance.unavailable = Some("zero or numerically degenerate coefficient variance".into());
    }
    let estimates = [ols.alpha, ols.beta];
    let se = [covariance.mean_se(&[1., 0.]), covariance.mean_se(&[0., 1.])];
    let intervals = std::array::from_fn(|i| {
        estimates[i].zip(se[i]).filter(|(_, s)| *s > 0.).map(|(v, s)| (v - 1.96 * s, v + 1.96 * s))
    });
    let p = std::array::from_fn(|i| {
        estimates[i].zip(se[i]).filter(|(_, s)| *s > 0.).map(|(v, s)| inference::normal_p((v - i as f64) / s))
    });
    let family = crate::nulls::Family::by("CORR alpha=0; beta=1", &p);
    FitInference { covariance, p, q: [family.q[0], family.q[1]], intervals }
}

pub fn chronological_holdout(pairs: &[PairReturn], annual_periods: f64) -> Holdout {
    chronological_holdout_h(pairs, annual_periods, 1)
}

/// 80/20 chronological holdout for h-bar overlapping pairs. The last h−1 nominal training pairs
/// share bars with the first test pair, so they are purged from the fit and the baseline mean
/// (slice-5 correction; at h=1 nothing is purged and results are unchanged).
pub fn chronological_holdout_h(pairs: &[PairReturn], annual_periods: f64, horizon: usize) -> Holdout {
    let nominal = pairs.len() * 4 / 5;
    let test_n = pairs.len() - nominal;
    let purged = horizon.saturating_sub(1).min(nominal);
    let train_n = nominal - purged;
    let mut out = Holdout {
        train_n,
        test_n,
        purged,
        split_ts: pairs.get(nominal).map_or(0, |p| p.end),
        alpha: None,
        beta: None,
        train_mean: None,
        r_squared_oos: None,
        unavailable: None,
    };
    if train_n < 30 || test_n < 10 {
        out.unavailable = Some("holdout needs ≥30 training (after purge) and ≥10 test returns".into());
        return out;
    }
    let fit = regress(&pairs[..train_n], annual_periods).expect("admitted finite returns");
    out.alpha = fit.alpha;
    out.beta = fit.beta;
    let mean = pairs[..train_n].iter().map(|p| p.asset).sum::<f64>() / train_n as f64;
    out.train_mean = Some(mean);
    if let (Some(a), Some(b)) = (fit.alpha, fit.beta) {
        let sse = pairs[nominal..].iter().map(|p| (p.asset - a - b * p.benchmark).powi(2)).sum();
        let baseline = pairs[nominal..].iter().map(|p| (p.asset - mean).powi(2)).sum::<f64>();
        out.r_squared_oos = divide(sse, baseline).and_then(|r| finite(1. - r));
    }
    if out.r_squared_oos.is_none() {
        out.unavailable = Some("singular training fit or zero test baseline variance".into());
    }
    out
}

#[cfg(test)]
fn robust_report(pairs: &[PairReturn], ols: &Ols, annual_periods: f64) -> RobustReport {
    robust_report_horizon(pairs, ols, annual_periods, 1)
}
fn robust_report_horizon(pairs: &[PairReturn], ols: &Ols, annual_periods: f64, horizon: usize) -> RobustReport {
    let breaks = pair_breaks(pairs);
    let effective = |method| {
        if horizon > 1 && matches!(method, CovarianceMethod::HacFixed5 | CovarianceMethod::HacAutomatic) {
            CovarianceMethod::Overlap(horizon)
        } else {
            method
        }
    };
    let fits = SE_METHODS.iter().map(|&method| fit_inference(pairs, ols, &breaks, effective(method))).collect();
    let mut rolling = vec![Vec::new(); SE_METHODS.len()];
    // Lean rolling refits: one residual buffer, no per-window `Ols`/Family/design allocations.
    // Arithmetic matches `regress` + `fit_inference` operation for operation (bit-identical).
    let mut residual = Vec::with_capacity(60);
    let mut labels: Vec<(CovarianceMethod, usize, bool, String)> = Vec::new();
    for end in 60.max(pairs.len().saturating_sub(999))..=pairs.len() {
        let start = end - 60;
        let w = &pairs[start..end];
        let crosses = breaks[start + 1..end].iter().any(|b| *b);
        let core = (!crosses).then(|| core_fit(w));
        if let Some(core) = &core {
            residual.clear();
            residual.extend(
                w.iter().map(|p| p.asset - core.alpha.unwrap_or(0.) - core.beta.unwrap_or(0.) * p.benchmark),
            );
        }
        let magnitude = w.iter().map(|p| p.asset.abs()).fold(0., f64::max);
        for (i, &method) in SE_METHODS.iter().enumerate() {
            let (interval, label) = match &core {
                Some(core) => {
                    let (interval, method, lag, fallback) =
                        rolling_beta_interval(w, &breaks[start..end], &residual, magnitude, core, effective(method));
                    let at = labels.iter().position(|l| l.0 == method && l.1 == lag && l.2 == fallback);
                    let at = at.unwrap_or_else(|| {
                        labels.push((method, lag, fallback, method.label(lag, fallback)));
                        labels.len() - 1
                    });
                    (interval, labels[at].3.clone())
                }
                None => (None, "window crosses a vault join".to_string()),
            };
            rolling[i].push(BetaBandPoint {
                ts: w[59].end,
                start: w[0].start,
                beta: core.as_ref().and_then(|c| c.beta),
                covariance_label: label,
                interval,
                break_before: breaks[end - 1] || crosses,
            });
        }
    }
    RobustReport {
        fits,
        rolling,
        holdout: Some(chronological_holdout_h(pairs, annual_periods, horizon)),
        theil_sen: Some(theil_sen(pairs)),
    }
}

fn pair_breaks(pairs: &[PairReturn]) -> Vec<bool> {
    pairs.iter().enumerate().map(|(i, p)| i > 0 && pairs[i - 1].end < p.start).collect()
}

/// Same expressions and summation order as the head of [`regress`].
fn moments(w: &[PairReturn]) -> (f64, f64, f64, f64, f64) {
    let n = w.len();
    let mx = w.iter().map(|p| p.benchmark).sum::<f64>() / n as f64;
    let my = w.iter().map(|p| p.asset).sum::<f64>() / n as f64;
    let xx = w.iter().map(|p| (p.benchmark - mx).powi(2)).sum::<f64>();
    let yy = w.iter().map(|p| (p.asset - my).powi(2)).sum::<f64>();
    let xy = w.iter().map(|p| (p.benchmark - mx) * (p.asset - my)).sum::<f64>();
    (mx, my, xx, yy, xy)
}

/// β and Pearson ρ of one window exactly as `regress` computes them, without the
/// up/down-capture subsets. Windows are ≥ 3 admitted (finite) returns.
fn beta_corr(w: &[PairReturn]) -> (Option<f64>, Option<f64>) {
    let n = w.len();
    if n < 3 {
        return (None, None);
    }
    let (_, _, xx, yy, xy) = moments(w);
    let tolerance = n as f64 * (16.0 * f64::EPSILON).powi(2);
    if xx > tolerance {
        let correlation = if yy > tolerance {
            finite((xy / xx.sqrt() / yy.sqrt()).clamp(-1.0, 1.0))
        } else {
            None
        };
        (finite(xy / xx), correlation)
    } else {
        (None, None)
    }
}

struct CoreFit {
    alpha: Option<f64>,
    beta: Option<f64>,
    sigma: Option<f64>,
}

/// α, β and regression σ of one window, as `regress` computes them.
fn core_fit(w: &[PairReturn]) -> CoreFit {
    let n = w.len();
    let mut out = CoreFit { alpha: None, beta: None, sigma: None };
    if n < 3 {
        return out;
    }
    let (mx, my, xx, _, xy) = moments(w);
    let tolerance = n as f64 * (16.0 * f64::EPSILON).powi(2);
    if xx > tolerance {
        let beta = xy / xx;
        let alpha = my - beta * mx;
        let variance = w.iter().map(|p| (p.asset - alpha - beta * p.benchmark).powi(2)).sum::<f64>() / (n - 2) as f64;
        out.alpha = finite(alpha);
        out.beta = finite(beta);
        out.sigma = finite(variance.sqrt());
    }
    out
}

/// The 95% β interval `fit_inference` would report for this window, plus the effective
/// method, lag and fallback flag that label it. No testing-family work: rolling points
/// never use p or q.
fn rolling_beta_interval(
    w: &[PairReturn],
    breaks: &[bool],
    residual: &[f64],
    magnitude: f64,
    core: &CoreFit,
    method: CovarianceMethod,
) -> (Option<(f64, f64)>, CovarianceMethod, usize, bool) {
    let mut covariance = inference::regression_covariance_two(w.len(), |t| [1., w[t].benchmark], residual, breaks, method);
    if core.alpha.is_none()
        || core.beta.is_none()
        || core.sigma.is_none_or(|s| s <= 64. * f64::EPSILON * magnitude)
    {
        covariance.covariance = None;
    }
    if covariance.covariance.as_ref().is_some_and(|v| v[0][0] <= 0. || v[1][1] <= 0.) {
        covariance.covariance = None;
    }
    let se = covariance.mean_se(&[0., 1.]);
    let interval = core.beta.zip(se).filter(|(_, s)| *s > 0.).map(|(v, s)| (v - 1.96 * s, v + 1.96 * s));
    (interval, covariance.method, covariance.lag, covariance.fallback)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorrReport {
    #[serde(default)]
    pub robust: RobustReport,
    #[serde(default = "one_horizon")]
    pub horizon: usize,
    pub symbol: String,
    pub benchmark: String,
    pub provenance: Provenance,
    pub source: String,
    /// Structured receipts for [asset, benchmark] (display and provider identity only).
    #[serde(default)]
    pub receipts: Vec<crate::source_receipt::SourceReceipt>,
    pub as_of: i64,
    #[serde(default)]
    pub closed_thru: i64,
    #[serde(default)]
    pub observed_at: Option<i64>,
    pub annual_periods: f64,
    pub aligned_bars: usize,
    pub asset_bars: usize,
    pub benchmark_bars: usize,
    pub returns: Vec<PairReturn>,
    pub ols: Ols,
    pub rolling: Vec<Rolling>,
    pub path: Vec<PathPoint>,
    pub matrix: Matrix,
    /// Vault gaps (asset or benchmark) inside the common range; no return spans one.
    pub gaps: Vec<(i64, i64)>,
    /// Relate transforms, expanding β, common panel and fair value. Absent for legacy CORR/RBETA,
    /// and omitted from serialization when absent so legacy exports are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relate: Option<Box<RelateReport>>,
}

/// Both entire input series must pass admission, even rows outside the intersection.
fn aligned(asset: &[Bar], benchmark: &[Bar], daily: bool) -> Result<Vec<(i64, f64, f64)>, String> {
    crate::stats::require_research_bars(asset, "Relationship asset")?;
    crate::stats::require_research_bars(benchmark, "Relationship benchmark")?;
    let key = |ts: i64| if daily { ts.div_euclid(86_400) } else { ts };
    if daily && [asset, benchmark].iter().any(|bars| bars.windows(2).any(|w| key(w[0].ts) == key(w[1].ts))) {
        return Err("Relationship refused: two daily bars share one calendar date".into());
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < asset.len() && j < benchmark.len() {
        match key(asset[i].ts).cmp(&key(benchmark[j].ts)) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push((asset[i].ts, asset[i].close.ln(), benchmark[j].close.ln()));
                i += 1;
                j += 1;
            }
        }
    }
    Ok(out)
}

/// Returns between adjacent common prices, skipping any pair that straddles a
/// vault gap in either series. Gap stamps are compared on the asset's clock,
/// which for daily joins can differ from the benchmark's by under a day, so a
/// daily gap check widens to whole dates.
fn returns(prices: &[(i64, f64, f64)], gaps: &[(i64, i64)]) -> Vec<PairReturn> {
    prices
        .windows(2)
        .filter(|w| !crate::studio_dist::spans_gap(gaps, w[0].0, w[1].0))
        .map(|w| PairReturn { start: w[0].0, end: w[1].0, asset: w[1].1 - w[0].1, benchmark: w[1].2 - w[0].2 })
        .collect()
}

fn joined_gaps(a: &NamedBars, b: &NamedBars, daily: bool) -> Vec<(i64, i64)> {
    let mut gaps: Vec<_> = a.gaps.iter().chain(&b.gaps).copied().collect();
    if daily {
        // Day-granular: the asset's stamp for a date may be later/earlier than the benchmark's.
        for g in &mut gaps {
            *g = (g.0.div_euclid(86_400) * 86_400 + 86_399, g.1.div_euclid(86_400) * 86_400);
        }
    }
    gaps.sort_unstable();
    gaps.dedup();
    gaps
}

fn finite(x: f64) -> Option<f64> {
    x.is_finite().then_some(x)
}
fn divide(x: f64, y: f64) -> Option<f64> {
    if y == 0.0 { None } else { finite(x / y) }
}

/// Classical homoskedastic OLS with an intercept; no risk-free rate or HAC correction.
/// Undefined statistics remain None, including t-statistics with zero standard error.
pub fn regress(pairs: &[PairReturn], annual_periods: f64) -> Result<Ols, String> {
    if !annual_periods.is_finite()
        || annual_periods <= 0.0
        || pairs.iter().any(|p| !p.asset.is_finite() || !p.benchmark.is_finite())
    {
        return Err("Relationship refused: non-finite returns or invalid annualisation".into());
    }
    let n = pairs.len();
    let mut out = Ols {
        n,
        alpha: None,
        alpha_annual: None,
        beta: None,
        r_squared: None,
        correlation: None,
        alpha_se: None,
        beta_se: None,
        alpha_t: None,
        beta_t: None,
        residual_sigma: None,
        residual_vol_annual: None,
        up_capture: None,
        down_capture: None,
        upside_beta: None,
        downside_beta: None,
        up_n: 0,
        down_n: 0,
    };
    if n < 3 {
        return Ok(out);
    }
    let mx = pairs.iter().map(|p| p.benchmark).sum::<f64>() / n as f64;
    let my = pairs.iter().map(|p| p.asset).sum::<f64>() / n as f64;
    let xx = pairs.iter().map(|p| (p.benchmark - mx).powi(2)).sum::<f64>();
    let yy = pairs.iter().map(|p| (p.asset - my).powi(2)).sum::<f64>();
    let xy = pairs.iter().map(|p| (p.benchmark - mx) * (p.asset - my)).sum::<f64>();
    let tolerance = n as f64 * (16.0 * f64::EPSILON).powi(2);
    if xx > tolerance {
        let beta = xy / xx;
        let alpha = my - beta * mx;
        let variance =
            pairs.iter().map(|p| (p.asset - alpha - beta * p.benchmark).powi(2)).sum::<f64>() / (n - 2) as f64;
        out.alpha = finite(alpha);
        out.alpha_annual = finite(alpha * annual_periods);
        out.beta = finite(beta);
        out.residual_sigma = finite(variance.sqrt());
        out.residual_vol_annual = finite((variance * annual_periods).sqrt());
        out.alpha_se = finite((variance * (1.0 / n as f64 + mx * mx / xx)).sqrt());
        out.beta_se = finite((variance / xx).sqrt());
        out.alpha_t = out.alpha_se.and_then(|se| divide(alpha, se));
        out.beta_t = out.beta_se.and_then(|se| divide(beta, se));
        if yy > tolerance {
            out.correlation = finite((xy / xx.sqrt() / yy.sqrt()).clamp(-1.0, 1.0));
            out.r_squared = out.correlation.map(|r| r * r);
        }
    }
    // Subset slopes do not recurse; their intercept is fitted separately.
    for up in [true, false] {
        let subset: Vec<_> = pairs.iter().filter(|p| if up { p.benchmark > 0.0 } else { p.benchmark < 0.0 }).collect();
        let count = subset.len();
        let capture = divide(subset.iter().map(|p| p.asset).sum(), subset.iter().map(|p| p.benchmark).sum());
        let beta = if count >= 3 {
            let x = subset.iter().map(|p| p.benchmark).sum::<f64>() / count as f64;
            let y = subset.iter().map(|p| p.asset).sum::<f64>() / count as f64;
            let xx = subset.iter().map(|p| (p.benchmark - x).powi(2)).sum::<f64>();
            if xx > count as f64 * (16.0 * f64::EPSILON).powi(2) {
                divide(subset.iter().map(|p| (p.benchmark - x) * (p.asset - y)).sum(), xx)
            } else {
                None
            }
        } else {
            None
        };
        if up {
            out.up_n = count;
            out.up_capture = capture;
            out.upside_beta = beta;
        } else {
            out.down_n = count;
            out.down_capture = capture;
            out.downside_beta = beta;
        }
    }
    Ok(out)
}

fn matrix(rows: &[NamedBars], daily: bool) -> Matrix {
    let errors: Vec<Option<String>> = rows
        .iter()
        .map(|r| r.error.clone().or_else(|| crate::stats::require_research_bars(&r.bars, "Correlation matrix").err()))
        .collect();
    let cells = rows
        .iter()
        .enumerate()
        .map(|(i, a)| {
            rows.iter()
                .enumerate()
                .map(|(j, b)| {
                    if let Some(reason) = errors[i].as_ref().or(errors[j].as_ref()) {
                        return MatrixCell { correlation: None, n: 0, reason: Some(reason.clone()) };
                    }
                    let gaps = joined_gaps(a, b, daily);
                    match aligned(&a.bars, &b.bars, daily).and_then(|p| regress(&returns(&p, &gaps), 252.0)) {
                        Ok(m) => MatrixCell {
                            correlation: m.correlation,
                            n: m.n,
                            reason: m
                                .correlation
                                .is_none()
                                .then(|| "Need ≥3 common returns and nonzero variance".into()),
                        },
                        Err(reason) => MatrixCell { correlation: None, n: 0, reason: Some(reason) },
                    }
                })
                .collect()
        })
        .collect();
    Matrix {
        symbols: rows.iter().map(|r| r.symbol.clone()).collect(),
        cells,
        unavailable: rows.iter().zip(errors).filter_map(|(r, e)| e.map(|e| (r.symbol.clone(), e))).collect(),
    }
}

/// Inputs contain completed bars only; worker admission owns the observation clock.
pub fn compute(
    symbol: &str,
    benchmark: &str,
    asset: &[Bar],
    other: &[Bar],
    annual_periods: f64,
    rows: &[NamedBars],
    source: &str,
) -> Result<CorrReport, String> {
    let named = |s: &str, bars: &[Bar]| NamedBars { symbol: s.into(), bars: bars.to_vec(), ..Default::default() };
    compute_series(
        &named(symbol, asset),
        &named(benchmark, other),
        rows,
        Options { annual_periods, daily: false },
        source,
    )
}

/// Trailing z-score window for the log price ratio (prior observations only).
pub const Z_WINDOW: usize = 60;

/// Full relationship report. Gaps on `asset`/`benchmark` (and on every matrix
/// row) are honoured: no return straddles a vault UNVERIFIED JOIN gap.
pub fn compute_series(
    asset: &NamedBars,
    benchmark: &NamedBars,
    rows: &[NamedBars],
    options: Options,
    source: &str,
) -> Result<CorrReport, String> {
    compute_series_at_horizon(asset, benchmark, rows, options, source, 1)
}

pub fn compute_series_at_horizon(
    asset: &NamedBars,
    benchmark: &NamedBars,
    rows: &[NamedBars],
    options: Options,
    source: &str,
    horizon: usize,
) -> Result<CorrReport, String> {
    if ![1, 5, 20].contains(&horizon) {
        return Err("CORR horizon must be 1, 5 or 20 observations".into());
    }
    let Options { annual_periods, daily } = options;
    let annual_periods = annual_periods / horizon as f64;
    let (symbol, other) = (&asset.symbol, &benchmark.bars);
    let prices = aligned(&asset.bars, other, daily)?;
    if prices.len() < 4 {
        return Err(
            "Relationship needs at least 4 common closed price timestamps (3 returns); no fill-forward is used.".into(),
        );
    }
    let all_gaps = joined_gaps(asset, benchmark, daily);
    let pairs = if horizon == 1 {
        returns(&prices, &all_gaps)
    } else {
        prices
            .windows(horizon + 1)
            .filter(|w| !crate::studio_dist::spans_gap(&all_gaps, w[0].0, w[horizon].0))
            .map(|w| PairReturn {
                start: w[0].0,
                end: w[horizon].0,
                asset: w[horizon].1 - w[0].1,
                benchmark: w[horizon].2 - w[0].2,
            })
            .collect()
    };
    if pairs.len() < 3 {
        return Err("Relationship needs at least 3 common returns that do not span a vault gap.".into());
    }
    let (first, last) = (prices[0].0, prices[prices.len() - 1].0);
    let gaps: Vec<_> = all_gaps.iter().copied().filter(|g| g.1 > first && g.0 < last).collect();
    let ols = regress(&pairs, annual_periods)?;
    let robust = robust_report_horizon(&pairs, &ols, annual_periods, horizon);
    // `pairs` already passed `regress`, so every window is finite. A window is broken when any
    // adjacent pair inside it is non-contiguous: an O(1) prefix-count check per window.
    let breaks = pair_breaks(&pairs);
    let mut prefix = Vec::with_capacity(breaks.len() + 1);
    prefix.push(0usize);
    prefix.extend(breaks.iter().scan(0usize, |c, b| {
        *c += usize::from(*b);
        Some(*c)
    }));
    let rolling = ROLLING_WINDOWS
        .iter()
        .map(|&window| {
            let points = pairs
                .windows(window)
                .enumerate()
                .map(|(s, w)| {
                    let broken = prefix[s + window] - prefix[s + 1] > 0;
                    let (beta, correlation) = beta_corr(w);
                    (w[window - 1].end, if broken { None } else { beta }, if broken { None } else { correlation })
                })
                .collect();
            Rolling { window, points }
        })
        .collect::<Vec<_>>();
    let mut cumulative = 0.0;
    let mut next_pair = 0;
    let path = prices
        .iter()
        .enumerate()
        .map(|(i, &(ts, a, b))| {
            let log_ratio = a - b;
            // Strictly trailing: mean/sd of the previous Z_WINDOW ratios, not including today.
            let z60 = if i >= Z_WINDOW && !crate::studio_dist::spans_gap(&all_gaps, prices[i - Z_WINDOW].0, ts) {
                let w = &prices[i - Z_WINDOW..i];
                let n = Z_WINDOW as f64;
                let mean = w.iter().map(|p| p.1 - p.2).sum::<f64>() / n;
                let sd = (w.iter().map(|p| (p.1 - p.2 - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
                if sd > 64.0 * f64::EPSILON * mean.abs().max(1.0) { divide(log_ratio - mean, sd) } else { None }
            } else {
                None
            };
            // At h>1 the pairs are overlapping h-bar returns; summing their residuals would
            // count each one-bar residual about h times, so the path is unavailable.
            let residual_return = match (ols.alpha, ols.beta) {
                (Some(alpha), Some(beta)) if horizon == 1 => {
                    // Pairs are a gap-filtered subsequence; match by end timestamp.
                    if pairs.get(next_pair).is_some_and(|p| p.end == ts) {
                        let p = &pairs[next_pair];
                        cumulative += p.asset - alpha - beta * p.benchmark;
                        next_pair += 1;
                    }
                    finite(cumulative.exp_m1())
                }
                _ => None,
            };
            PathPoint { ts, log_ratio, z60, residual_return }
        })
        .collect();
    Ok(CorrReport {
        robust,
        horizon,
        symbol: symbol.clone(),
        benchmark: benchmark.symbol.clone(),
        provenance: Bar::provenance(&asset.bars).combine(Bar::provenance(other)),
        source: source.into(),
        receipts: [asset, benchmark].into_iter().filter_map(|r| r.receipt.clone()).collect(),
        as_of: prices.last().unwrap().0,
        closed_thru: prices.last().unwrap().0,
        observed_at: None,
        annual_periods,
        aligned_bars: prices.len(),
        asset_bars: asset.bars.len(),
        benchmark_bars: other.len(),
        returns: pairs,
        ols,
        rolling,
        path,
        matrix: matrix(rows, daily),
        gaps,
        relate: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bars(log_returns: &[f64]) -> Vec<Bar> {
        let mut log = 4.0;
        std::iter::once(0.0)
            .chain(log_returns.iter().copied())
            .enumerate()
            .map(|(i, r)| {
                log += r;
                let p = log.exp();
                Bar::new(i as i64 * 86400, p, p, p, p, 1.0).with_provenance(Provenance::Real)
            })
            .collect()
    }
    fn near(a: Option<f64>, b: f64) {
        assert!((a.unwrap() - b).abs() < 1e-9, "{a:?} != {b}");
    }
    fn sample() -> (Vec<Bar>, Vec<Bar>) {
        let x = [-0.02, -0.01, 0.0, 0.01, 0.02];
        let e = [0.001, -0.002, 0.002, -0.002, 0.001];
        (bars(&x.iter().zip(e).map(|(x, e)| 0.001 + 2.0 * x + e).collect::<Vec<_>>()), bars(&x))
    }
    #[test]
    fn known_ols_and_standard_errors() {
        let (a, b) = sample();
        let r = compute("A", "B", &a, &b, 252.0, &[], "unit").unwrap();
        near(r.ols.alpha, 0.001);
        near(r.ols.alpha_annual, 0.252);
        near(r.ols.beta, 2.0);
        near(r.ols.residual_sigma, (14e-6_f64 / 3.0).sqrt());
        near(r.ols.beta_se, (14e-6_f64 / 3.0 / 0.001).sqrt());
        near(r.ols.alpha_se, (14e-6_f64 / 3.0 / 5.0).sqrt());
        near(r.ols.beta_t, 2.0 / (14e-6_f64 / 3.0 / 0.001).sqrt());
        near(r.ols.alpha_t, 0.001 / (14e-6_f64 / 3.0 / 5.0).sqrt());
        near(r.ols.r_squared, 0.004 / 0.004014);
        near(r.ols.correlation, (0.004_f64 / 0.004014).sqrt());
        near(r.ols.up_capture, 0.061 / 0.03);
        near(r.ols.down_capture, -0.059 / -0.03);
        near(r.path.last().unwrap().residual_return, 0.0);
    }
    #[test]
    fn alignment_intersects_prices_before_returns() {
        let (mut a, b) = sample();
        a.remove(2);
        let r = compute("A", "B", &a, &b, 252.0, &[], "unit").unwrap();
        assert_eq!(r.aligned_bars, 5);
        assert_eq!(r.returns.len(), 4);
        assert_eq!((r.returns[1].start, r.returns[1].end), (86400, 3 * 86400));
        assert!((r.returns[1].benchmark + 0.01).abs() < 1e-12);
        let mut shifted = b.clone();
        for p in &mut shifted {
            p.ts += 1;
        }
        assert!(compute("A", "B", &a, &shifted, 252.0, &[], "unit").is_err());
    }
    #[test]
    fn refuses_bad_inputs_even_outside_intersection() {
        let (a, b) = sample();
        for provenance in [Provenance::Sample, Provenance::Missing] {
            let mut bad = a.clone();
            bad[0].provenance = provenance;
            assert!(compute("A", "B", &bad, &b, 252.0, &[], "unit").is_err());
        }
        for bad_value in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            let mut bad = a.clone();
            bad[0].close = bad_value;
            assert!(compute("A", "B", &bad, &b, 252.0, &[], "unit").is_err());
        }
        let mut dup = a.clone();
        dup[1].ts = dup[0].ts;
        assert!(compute("A", "B", &dup, &b, 252.0, &[], "unit").is_err());
        assert!(compute("A", "B", &a[..3], &b, 252.0, &[], "unit").is_err());
        assert!(compute("A", "B", &[], &b, 252.0, &[], "unit").is_err());
        assert!(compute("A", "B", &a, &b, f64::NAN, &[], "unit").is_err());
        assert!(regress(&[PairReturn { start: 0, end: 1, asset: f64::NAN, benchmark: 0.0 }], 252.0).is_err());
    }
    #[test]
    fn zero_variance_is_unavailable_not_nan() {
        let flat = bars(&[0.0; 65]);
        let (_, b) = sample();
        let r = compute("A", "B", &flat, &flat, 252.0, &[], "unit").unwrap();
        assert_eq!(r.ols.beta, None);
        assert_eq!(r.ols.correlation, None);
        assert_eq!(r.path.last().unwrap().z60, None);
        assert_eq!(r.path.last().unwrap().residual_return, None);
        let r = compute("A", "B", &flat[..6], &b, 252.0, &[], "unit").unwrap();
        near(r.ols.beta, 0.0);
        assert_eq!(r.ols.r_squared, None);
        assert_eq!(r.ols.beta_t, None);
        assert!(!serde_json::to_string(&r).unwrap().contains("NaN"));
        // Constant nonzero log returns can differ at machine precision after logging prices.
        let constant = bars(&[0.01; 65]);
        let r = compute("A", "B", &flat, &constant, 252.0, &[], "unit").unwrap();
        assert_eq!(r.ols.beta, None);
        assert_eq!(r.ols.upside_beta, None);
    }
    #[test]
    fn rolling_capture_and_zscore_known_answers() {
        let x: Vec<_> = (0..130).map(|i| (i % 10) as f64 * 0.003 - 0.015).collect();
        let a = bars(&x.iter().map(|r| 2.0 * r).collect::<Vec<_>>());
        let b = bars(&x);
        let r = compute("A", "B", &a, &b, 252.0, &[], "unit").unwrap();
        for roll in &r.rolling {
            assert_eq!(roll.points.len(), 131 - roll.window);
            for p in &roll.points {
                near(p.1, 2.0);
                near(p.2, 1.0);
            }
        }
        near(r.ols.upside_beta, 2.0);
        near(r.ols.downside_beta, 2.0);
        near(r.ols.up_capture, 2.0);
        near(r.ols.down_capture, 2.0);
        assert!(r.path[59].z60.is_none());
        assert!(r.path[60].z60.is_some());
        let ascending = bars(&[0.01; 65]);
        let z = compute("A", "B", &ascending, &bars(&[0.0; 65]), 252.0, &[], "unit").unwrap();
        // Ratio i×0.01; z at i=60 uses only ratios 0..59: mean 0.295, sd 0.01×√(60·61/12).
        near(z.path[60].z60, 30.5 / (60.0_f64 * 61.0 / 12.0).sqrt());
    }
    #[test]
    fn ratio_z_score_is_strictly_trailing() {
        // Appending future prices must not change any earlier z-score.
        let x: Vec<_> = (0..140).map(|i| ((i * 13 % 17) as f64 - 8.0) * 0.002).collect();
        let a = bars(&x.iter().map(|r| r * 1.5 + 0.0005).collect::<Vec<_>>());
        let b = bars(&x);
        let full = compute("A", "B", &a, &b, 252.0, &[], "unit").unwrap();
        let part = compute("A", "B", &a[..90], &b[..90], 252.0, &[], "unit").unwrap();
        for (p, f) in part.path.iter().zip(&full.path) {
            assert_eq!(p.z60, f.z60);
        }
    }
    #[test]
    fn daily_join_is_by_date_and_vault_gaps_break_returns() {
        let (a, b) = sample();
        // Benchmark stamped at 21:00 UTC on the same dates: a date join, not an index or exact-ts join.
        let late: Vec<Bar> = b.iter().map(|x| Bar { ts: x.ts + 75_600, ..*x }).collect();
        let named = |s: &str, bars: &[Bar], gaps: Vec<(i64, i64)>| NamedBars {
            symbol: s.into(),
            bars: bars.to_vec(),
            gaps,
            ..Default::default()
        };
        let opts = Options { annual_periods: 252.0, daily: true };
        let r = compute_series(&named("A", &a, vec![]), &named("B", &late, vec![]), &[], opts, "unit").unwrap();
        near(r.ols.beta, 2.0);
        assert_eq!(r.ols.n, 5);
        // Intraday semantics (exact timestamps) find no common bars.
        let intraday = Options { daily: false, ..opts };
        assert!(compute_series(&named("A", &a, vec![]), &named("B", &late, vec![]), &[], intraday, "unit").is_err());
        // A vault gap on the benchmark between bars 2 and 3 removes exactly that return.
        let gap = (late[2].ts, late[3].ts);
        let r = compute_series(&named("A", &a, vec![]), &named("B", &late, vec![gap]), &[], opts, "unit").unwrap();
        assert_eq!(r.ols.n, 4);
        assert_eq!(r.gaps.len(), 1);
        assert!(r.returns.iter().all(|p| !(p.start == a[2].ts && p.end == a[3].ts)));
        // Matrix rows honour row gaps too.
        let rows = vec![named("A", &a, vec![(a[1].ts, a[2].ts)]), named("B", &b, vec![])];
        assert_eq!(matrix(&rows, false).cells[0][1].n, 4);
    }
    #[test]
    fn matrix_keeps_missing_symbols_and_pair_counts() {
        let (a, b) = sample();
        let row = |s: &str, bars: Vec<Bar>, error: Option<String>| NamedBars {
            symbol: s.into(),
            bars,
            source: "unit".into(),
            error,
            gaps: vec![],
            receipt: None,
        };
        let rows =
            vec![row("A", a.clone(), None), row("B", b.clone(), None), row("TLT", vec![], Some("no archive".into()))];
        let m = matrix(&rows, false);
        assert_eq!(m.symbols, ["A", "B", "TLT"]);
        assert_eq!(m.cells[0][1].n, 5);
        near(m.cells[0][0].correlation, 1.0);
        near(m.cells[0][1].correlation, m.cells[1][0].correlation.unwrap());
        assert_eq!(m.unavailable, [("TLT".into(), "no archive".into())]);
        assert_eq!(m.cells[2][2].correlation, None);
    }
}

#[path = "studio_relate.rs"]
pub(crate) mod relate;
pub use relate::*;

#[cfg(test)]
#[path = "studio_corr_fast_tests.rs"]
mod fast_path_tests;
#[cfg(test)]
#[path = "studio_relate_tests.rs"]
mod relate_tests;

#[cfg(test)]
mod robust_tests {
    use super::*;
    fn pairs(n: usize) -> Vec<PairReturn> {
        (0..n)
            .map(|i| {
                let x = ((i * 7 % 19) as f64 - 9.) * 0.001;
                PairReturn {
                    start: i as i64,
                    end: i as i64 + 1,
                    benchmark: x,
                    asset: 0.001 + 1.5 * x + ((i % 7) as f64 - 3.) * 0.0002,
                }
            })
            .collect()
    }
    #[test]
    fn ols_point_estimates_unchanged() {
        let p = pairs(100);
        let ols = regress(&p, 252.).unwrap();
        let r = robust_report(&p, &ols, 252.);
        assert_eq!(ols, regress(&p, 252.).unwrap());
        assert_eq!(r.fits.len(), 4);
        let v = r.fits[0].covariance.covariance.as_ref().unwrap();
        assert!((v[0][0].sqrt() - ols.alpha_se.unwrap()).abs() < 1e-12);
        assert!((v[1][1].sqrt() - ols.beta_se.unwrap()).abs() < 1e-12);
        for fit in &r.fits {
            assert!(fit.intervals.iter().all(Option::is_some));
            assert_eq!(fit.q, crate::nulls::benjamini_yekutieli(&fit.p.map(|p| p.unwrap_or(1.))).as_slice());
        }
    }
    #[test]
    fn holdout_never_refits_and_r2_negative_on_noise() {
        let mut p = pairs(100);
        let original = chronological_holdout(&p, 252.);
        for p in &mut p[80..] {
            p.asset = -5. * p.benchmark;
        }
        let held = chronological_holdout(&p, 252.);
        assert_eq!((held.alpha, held.beta, held.train_mean), (original.alpha, original.beta, original.train_mean));
        assert!(held.r_squared_oos.unwrap() < 0.);
        let (a, b) = (held.alpha.unwrap(), held.beta.unwrap());
        let mean = held.train_mean.unwrap();
        let reference = 1.
            - p[80..].iter().map(|p| (p.asset - a - b * p.benchmark).powi(2)).sum::<f64>()
                / p[80..].iter().map(|p| (p.asset - mean).powi(2)).sum::<f64>();
        assert_eq!(held.r_squared_oos, Some(reference));
        assert!(chronological_holdout(&p[..35], 252.).r_squared_oos.is_none());
    }
    #[test]
    fn holdout_purges_last_h_minus_1_training_pairs() {
        let p = pairs(100);
        let h1 = chronological_holdout_h(&p, 252., 1);
        assert_eq!((h1.train_n, h1.purged, h1.test_n), (80, 0, 20));
        assert_eq!(h1, chronological_holdout(&p, 252.));
        for (h, purged) in [(5usize, 4usize), (20, 19)] {
            let held = chronological_holdout_h(&p, 252., h);
            assert_eq!((held.train_n, held.purged, held.test_n), (80 - purged, purged, 20));
            // The test block and the split stamp stay at the nominal boundary.
            assert_eq!(held.split_ts, h1.split_ts);
            // Frozen fit uses exactly the surviving training pairs.
            let fit = regress(&p[..80 - purged], 252.).unwrap();
            assert_eq!((held.alpha, held.beta), (fit.alpha, fit.beta));
            let mean = p[..80 - purged].iter().map(|q| q.asset).sum::<f64>() / (80 - purged) as f64;
            assert_eq!(held.train_mean, Some(mean));
        }
        // Too few surviving training pairs is refused, never silently relabelled.
        assert!(chronological_holdout_h(&p[..38], 252., 20).unavailable.is_some());
    }
    #[test]
    fn appending_bars_keeps_earlier_rolling_estimates() {
        let p = pairs(170);
        let a = robust_report(&p[..150], &regress(&p[..150], 252.).unwrap(), 252.);
        let b = robust_report(&p, &regress(&p, 252.).unwrap(), 252.);
        for (a, b) in a.rolling.iter().zip(&b.rolling) {
            assert_eq!(a, &b[..a.len()]);
        }
        let p = pairs(1300);
        let r = robust_report(&p, &regress(&p, 252.).unwrap(), 252.);
        assert!(r.rolling.iter().all(|r| r.len() == 1000));
    }
    #[test]
    fn no_lag_product_or_polygon_across_vault_gap() {
        let mut p = pairs(150);
        for p in &mut p[70..] {
            p.start += 10;
            p.end += 10;
        }
        let r = robust_report(&p, &regress(&p, 252.).unwrap(), 252.);
        for band in &r.rolling {
            for point in band {
                if point.start < 80 && point.ts > 70 {
                    assert!(point.interval.is_none() && point.break_before);
                }
            }
        }
    }
}

#[cfg(test)]
mod horizon_tests {
    use super::*;
    #[test]
    fn theil_sen_ignores_single_outlier() {
        let mut p: Vec<_> = (0..101)
            .map(|i| PairReturn { start: i, end: i + 1, benchmark: i as f64, asset: 2. + 3. * i as f64 })
            .collect();
        p[50].asset = 1e9;
        assert_eq!(theil_sen(&p).slope, Some(3.));
        assert_eq!(theil_sen(&p).slopes, 5050);
        assert!(theil_sen(&[]).slope.is_none());
        for p in &mut p {
            p.benchmark = 1.;
        }
        assert!(theil_sen(&p).slope.is_none());
    }
    #[test]
    fn horizons_preserve_one_bar_ols_and_use_uniform_overlap_without_gap_windows() {
        let bars = |scale: f64| {
            (0..150)
                .map(|i| {
                    let p = (4. + scale * (0.001 * i as f64 + (i % 7) as f64 * 0.0001)).exp();
                    Bar::new(i * 1800, p, p, p, p, 10.).with_provenance(Provenance::Real)
                })
                .collect()
        };
        let mut a = NamedBars { symbol: "A".into(), bars: bars(1.2), ..Default::default() };
        let b = NamedBars { symbol: "B".into(), bars: bars(0.9), ..Default::default() };
        let options = Options { annual_periods: 3276., daily: false };
        let one = compute_series(&a, &b, &[], options, "test").unwrap();
        let explicit = compute_series_at_horizon(&a, &b, &[], options, "test", 1).unwrap();
        assert_eq!(one, explicit);
        a.gaps = vec![(a.bars[69].ts, a.bars[70].ts)];
        for h in [5, 20] {
            let r = compute_series_at_horizon(&a, &b, &[], options, "test", h).unwrap();
            assert_eq!(r.horizon, h);
            assert_eq!(r.ols.n, 150 - 2 * h);
            assert_eq!(r.robust.fits[2].covariance.method, CovarianceMethod::Overlap(h));
            assert_eq!(r.robust.fits[2].covariance.lag, h - 1);
            assert!(r.returns.iter().all(|p| !crate::studio_dist::spans_gap(&a.gaps, p.start, p.end)));
            for p in &r.robust.rolling[2] {
                if p.start < a.gaps[0].1 && p.ts > a.gaps[0].0 {
                    assert!(p.interval.is_none());
                }
            }
            assert!((r.returns[0].asset - (a.bars[h].close / a.bars[0].close).ln()).abs() < 1e-12);
        }
        assert!(compute_series_at_horizon(&a, &b, &[], options, "test", 2).is_err());
    }
    #[test]
    fn old_report_json_defaults_to_one_bar_and_unavailable_robust() {
        let p: Vec<_> = (0..4)
            .map(|i| {
                let v = 100. + i as f64;
                Bar::new(i, v, v, v, v, 1.).with_provenance(Provenance::Real)
            })
            .collect();
        let r = compute("A", "B", &p, &p, 252., &[], "test").unwrap();
        let mut old = serde_json::to_value(r).unwrap();
        old.as_object_mut().unwrap().remove("horizon");
        old.as_object_mut().unwrap().remove("robust");
        let restored: CorrReport = serde_json::from_value(old).unwrap();
        assert_eq!(restored.horizon, 1);
        assert!(restored.robust.fits.is_empty());
    }
}
