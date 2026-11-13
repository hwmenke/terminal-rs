//! Relationship Studio: verified, timestamp-aligned log returns and classical OLS.
//! No fill-forward. Returns span adjacent common price endpoints, including gaps.
use serde::{Deserialize, Serialize};
use tr_core::{Bar, Provenance};

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
pub struct CorrReport {
    pub symbol: String,
    pub benchmark: String,
    pub provenance: Provenance,
    pub source: String,
    pub as_of: i64,
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
}

/// Both entire input series must pass admission, even rows outside the intersection.
fn aligned(asset: &[Bar], benchmark: &[Bar], daily: bool) -> Result<Vec<(i64, f64, f64)>, String> {
    crate::stats::require_research_bars(asset, "Relationship asset")?;
    crate::stats::require_research_bars(benchmark, "Relationship benchmark")?;
    let key = |ts: i64| if daily { ts.div_euclid(86_400) } else { ts };
    if daily
        && [asset, benchmark].iter().any(|bars| bars.windows(2).any(|w| key(w[0].ts) == key(w[1].ts)))
    {
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
    let Options { annual_periods, daily } = options;
    let (symbol, other) = (&asset.symbol, &benchmark.bars);
    let prices = aligned(&asset.bars, other, daily)?;
    if prices.len() < 4 {
        return Err(
            "Relationship needs at least 4 common closed price timestamps (3 returns); no fill-forward is used.".into(),
        );
    }
    let all_gaps = joined_gaps(asset, benchmark, daily);
    let pairs = returns(&prices, &all_gaps);
    if pairs.len() < 3 {
        return Err("Relationship needs at least 3 common returns that do not span a vault gap.".into());
    }
    let (first, last) = (prices[0].0, prices[prices.len() - 1].0);
    let gaps: Vec<_> = all_gaps.iter().copied().filter(|g| g.1 > first && g.0 < last).collect();
    let ols = regress(&pairs, annual_periods)?;
    let rolling = ROLLING_WINDOWS
        .iter()
        .map(|&window| {
            let points = pairs
                .windows(window)
                .map(|w| {
                    let m = regress(w, annual_periods)?;
                    Ok((w[window - 1].end, m.beta, m.correlation))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(Rolling { window, points })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut cumulative = 0.0;
    let mut next_pair = 0;
    let path = prices
        .iter()
        .enumerate()
        .map(|(i, &(ts, a, b))| {
            let log_ratio = a - b;
            // Strictly trailing: mean/sd of the previous Z_WINDOW ratios, not including today.
            let z60 = if i >= Z_WINDOW {
                let w = &prices[i - Z_WINDOW..i];
                let n = Z_WINDOW as f64;
                let mean = w.iter().map(|p| p.1 - p.2).sum::<f64>() / n;
                let sd = (w.iter().map(|p| (p.1 - p.2 - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
                if sd > 64.0 * f64::EPSILON * mean.abs().max(1.0) { divide(log_ratio - mean, sd) } else { None }
            } else {
                None
            };
            let residual_return = match (ols.alpha, ols.beta) {
                (Some(alpha), Some(beta)) => {
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
        symbol: symbol.clone(),
        benchmark: benchmark.symbol.clone(),
        provenance: Bar::provenance(&asset.bars).combine(Bar::provenance(other)),
        source: source.into(),
        as_of: prices.last().unwrap().0,
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
        };
        let rows = vec![row("A", a.clone(), None), row("B", b.clone(), None), row("TLT", vec![], Some("no archive".into()))];
        let m = matrix(&rows, false);
        assert_eq!(m.symbols, ["A", "B", "TLT"]);
        assert_eq!(m.cells[0][1].n, 5);
        near(m.cells[0][0].correlation, 1.0);
        near(m.cells[0][1].correlation, m.cells[1][0].correlation.unwrap());
        assert_eq!(m.unavailable, [("TLT".into(), "no archive".into())]);
        assert_eq!(m.cells[2][2].correlation, None);
    }
}
