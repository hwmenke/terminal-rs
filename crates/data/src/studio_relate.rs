//! Relate transforms, expanding β, the common-panel matrix and the frozen-training fair-value
//! overlay (Studio slice 4). Child module of `studio_corr`; everything here is gated on a
//! `RelateSpec` and never runs for the legacy CORR/RBETA report.
//!
//! Conventions: *strict* fixed-bar transforms. A transform at row `t` over `h` bars exists only
//! when both series' own bars are `h` apart, no expected-grid break or vault gap lies inside
//! `(t-h, t]`, and the legacy common-timestamp adjacency is not used. Bands are residual ±kσ added
//! in **target space** and then inverted; nothing here cumulates residuals or issues a verdict.
use super::*;
use crate::studio::{BetaMode, MatrixMode, ScalarSeries, SeriesReceipt, SeriesUnit, Transform};
use tr_core::Timeframe;

/// Latest rows kept in an immutable report (bounds memory and paint work).
pub const MAX_POINTS: usize = 5_000;
/// Latest expanding-β endpoints kept.
pub const MAX_EXPANDING: usize = 1_000;
/// Chronological training share, as `chronological_holdout`.
pub const TRAIN_NUM: usize = 4;
pub const TRAIN_DEN: usize = 5;
pub const MIN_TRAIN: usize = 30;
pub const METHOD_SCHEMA: &str = "relate.v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelateSpec {
    /// Benchmark security key.
    pub benchmark: String,
    /// `h` inside Change/LogReturn is one of 1, 5, 20.
    pub transform: Transform,
    pub beta_mode: BetaMode,
    /// Used for the mean-response CI only; h>1 transforms use the labelled overlap kernel.
    pub covariance: CovarianceMethod,
    pub matrix: MatrixMode,
    pub bands: bool,
}
impl RelateSpec {
    pub fn new(benchmark: impl Into<String>, transform: Transform) -> Self {
        Self {
            benchmark: benchmark.into(),
            transform,
            beta_mode: BetaMode::Rolling,
            covariance: CovarianceMethod::HacFixed5,
            matrix: MatrixMode::Pairwise,
            bands: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FairValuePoint {
    pub ts: i64,
    /// Conditional centre (a conditional median for LogLevel), in price units.
    pub centre: f64,
    /// −2σ, −1σ, +1σ, +2σ residual bands, added in target space then inverted.
    pub bands: [f64; 4],
    pub break_before: bool,
    pub in_sample: bool,
    /// Regressor in target space: X, ln X, ΔX or r^X.
    #[serde(default)]
    pub x: f64,
    /// Realised target in target space (descriptive z uses it).
    #[serde(default)]
    pub y: f64,
    /// Realised asset price P_t.
    #[serde(default)]
    pub price: f64,
    /// P_{t−h} for the Change / LogReturn projections.
    #[serde(default)]
    pub anchor: Option<f64>,
    /// Descriptive z = (y − ŷ)/σ in target space. Not a verdict.
    #[serde(default)]
    pub z: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FairValueFit {
    pub fit_id: u64,
    pub transform: Transform,
    pub train_end: i64,
    pub n: usize,
    pub alpha: f64,
    pub beta: f64,
    /// √(SSE/(n−2)) in target space.
    pub sigma: f64,
    /// For the mean-response CI only.
    pub covariance: RegressionCovariance,
    pub unit: SeriesUnit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelateReport {
    pub spec: RelateSpec,
    pub key: u64,
    pub y: ScalarSeries,
    pub x: ScalarSeries,
    /// At most the latest 1,000 endpoints; descriptive, no uncertainty.
    pub expanding: Vec<(i64, Option<f64>)>,
    pub fit: Option<FairValueFit>,
    /// At most the latest 5,000 points.
    pub points: Vec<FairValuePoint>,
    pub matrix_common: Option<Matrix>,
    pub unavailable: Option<String>,
    /// Source of each side: asset, benchmark.
    pub sources: [String; 2],
    pub benchmark_gaps: Vec<(i64, i64)>,
}

/// Which rows of the aligned grid carry a transformed value.
pub struct Prepared {
    pub transform: Transform,
    pub ts: Vec<i64>,
    pub y: ScalarSeries,
    pub x: ScalarSeries,
    pub price: Vec<f64>,
    pub anchor: Vec<Option<f64>>,
}

struct Row {
    ts: i64,
    a: f64,
    b: f64,
    ai: usize,
    bi: usize,
}

/// Same admission and join rule as the legacy `aligned()`, keeping raw closes and bar indices.
fn aligned_indexed(asset: &[Bar], benchmark: &[Bar], daily: bool) -> Result<Vec<Row>, String> {
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
                out.push(Row { ts: asset[i].ts, a: asset[i].close, b: benchmark[j].close, ai: i, bi: j });
                i += 1;
                j += 1;
            }
        }
    }
    Ok(out)
}

fn prefix(breaks: &[bool]) -> Vec<usize> {
    let mut out = Vec::with_capacity(breaks.len() + 1);
    out.push(0);
    let mut c = 0;
    for b in breaks {
        c += usize::from(*b);
        out.push(c);
    }
    out
}

fn unit_for(transform: Transform) -> SeriesUnit {
    SeriesUnit::Native(
        match transform {
            Transform::Level => "price (native)",
            Transform::LogLevel => "ln price",
            Transform::Change { .. } => "Δ price (native)",
            Transform::LogReturn { .. } => "log-return",
        }
        .into(),
    )
}

/// Align, check the strict fixed-bar convention and transform. `tf` selects the expected grid.
#[allow(clippy::too_many_arguments)]
pub fn prepare(
    asset: &NamedBars,
    benchmark: &NamedBars,
    options: Options,
    tf: Timeframe,
    transform: Transform,
    source: &str,
    closed_thru: i64,
    observed_at: Option<i64>,
) -> Result<Prepared, String> {
    if ![1, 5, 20].contains(&transform.horizon()) {
        return Err("Relate h must be 1, 5 or 20".into());
    }
    let rows = aligned_indexed(&asset.bars, &benchmark.bars, options.daily)?;
    if rows.len() < 4 {
        return Err("Relate needs at least 4 common closed price timestamps; no fill-forward is used.".into());
    }
    let all_gaps = joined_gaps(asset, benchmark, options.daily);
    let cum_a = prefix(&crate::studio_canvas::expected_breaks_for(&asset.symbol, tf, &asset.bars, &asset.gaps));
    let cum_b = prefix(&crate::studio_canvas::expected_breaks_for(&benchmark.symbol, tf, &benchmark.bars, &benchmark.gaps));
    // Row t spans (t-h, t] cleanly: bars exactly h apart in each series, no expected-grid break
    // inside either, and no vault gap crossed.
    let span_ok = |t: usize, h: usize| {
        t >= h
            && rows[t].ai - rows[t - h].ai == h
            && rows[t].bi - rows[t - h].bi == h
            && cum_a[rows[t].ai + 1] == cum_a[rows[t - h].ai + 1]
            && cum_b[rows[t].bi + 1] == cum_b[rows[t - h].bi + 1]
            && !crate::studio_dist::spans_gap(&all_gaps, rows[t - h].ts, rows[t].ts)
    };
    let n = rows.len();
    let (mut y, mut x) = (vec![None; n], vec![None; n]);
    let mut brk = vec![false; n];
    let mut anchor = vec![None; n];
    let h = transform.horizon();
    for t in 0..n {
        let (p, q) = (rows[t].a, rows[t].b);
        match transform {
            Transform::Level => {
                (y[t], x[t]) = (finite(p), finite(q));
                brk[t] = !span_ok(t, 1);
            }
            Transform::LogLevel => {
                (y[t], x[t]) = (finite(p.ln()), finite(q.ln()));
                brk[t] = !span_ok(t, 1);
            }
            Transform::Change { .. } | Transform::LogReturn { .. } => {
                if span_ok(t, h) {
                    let (p0, q0) = (rows[t - h].a, rows[t - h].b);
                    let log = matches!(transform, Transform::LogReturn { .. });
                    let (dy, dx) = if log { (p.ln() - p0.ln(), q.ln() - q0.ln()) } else { (p - p0, q - q0) };
                    if let (Some(dy), Some(dx)) = (finite(dy), finite(dx)) {
                        (y[t], x[t]) = (Some(dy), Some(dx));
                        anchor[t] = Some(p0);
                    }
                }
            }
        }
    }
    if !transform.is_level() {
        for t in 0..n {
            brk[t] = y[t].is_some() && (t == 0 || y[t - 1].is_none());
        }
    }
    let provenance = Bar::provenance(&asset.bars).combine(Bar::provenance(&benchmark.bars));
    let receipt = |bars: &[Bar]| SeriesReceipt {
        source: source.into(),
        provenance,
        observed_at,
        closed_thru,
        input_digest: crate::stable_hash::bars_digest(bars),
        gaps: all_gaps.clone(),
    };
    let ts: Vec<i64> = rows.iter().map(|r| r.ts).collect();
    let unit = unit_for(transform);
    Ok(Prepared {
        transform,
        y: ScalarSeries {
            ts: ts.clone(),
            values: y,
            break_before: brk.clone(),
            unit: unit.clone(),
            receipt: receipt(&asset.bars),
        },
        x: ScalarSeries { ts: ts.clone(), values: x, break_before: brk, unit, receipt: receipt(&benchmark.bars) },
        ts,
        price: rows.iter().map(|r| r.a).collect(),
        anchor,
    })
}

impl Prepared {
    /// Rows carrying a transformed pair, oldest first.
    pub fn valid_rows(&self) -> Vec<usize> {
        (0..self.ts.len()).filter(|&t| self.y.values[t].is_some() && self.x.values[t].is_some()).collect()
    }
}

fn effective_method(method: CovarianceMethod, h: usize) -> CovarianceMethod {
    if h > 1 && matches!(method, CovarianceMethod::HacFixed5 | CovarianceMethod::HacAutomatic) {
        CovarianceMethod::Overlap(h)
    } else {
        method
    }
}

/// OLS on the chronological first 80% of valid pairs. σ = √(SSE/(n−2)) in target space.
pub fn training_fit(p: &Prepared, key: u64, method: CovarianceMethod) -> Result<FairValueFit, String> {
    let idx = p.valid_rows();
    let train_n = idx.len() * TRAIN_NUM / TRAIN_DEN;
    if train_n < MIN_TRAIN {
        return Err(format!(
            "Fair value needs ≥{MIN_TRAIN} training pairs; have {train_n} of {} valid transformed pairs.",
            idx.len()
        ));
    }
    let h = p.transform.horizon();
    let train = &idx[..train_n];
    let pairs: Vec<PairReturn> = train
        .iter()
        .map(|&t| PairReturn {
            start: if p.transform.is_level() { p.ts[t] } else { p.ts[t - h] },
            end: p.ts[t],
            asset: p.y.values[t].unwrap(),
            benchmark: p.x.values[t].unwrap(),
        })
        .collect();
    let ols = regress(&pairs, 252.)?;
    let (Some(alpha), Some(beta)) = (ols.alpha, ols.beta) else {
        return Err("Fair value unavailable: the benchmark has no variance in the training span.".into());
    };
    let magnitude = pairs.iter().map(|q| q.asset.abs()).fold(0., f64::max);
    let Some(sigma) = ols.residual_sigma.filter(|s| *s > 64. * f64::EPSILON * magnitude) else {
        return Err("Fair value unavailable: numerically zero residual variance in the training span.".into());
    };
    let residual: Vec<f64> = pairs.iter().map(|q| q.asset - alpha - beta * q.benchmark).collect();
    let breaks: Vec<bool> = (0..train_n).map(|i| i > 0 && (p.y.break_before[train[i]] || train[i] != train[i - 1] + 1)).collect();
    let covariance = inference::regression_covariance_two(
        train_n,
        |i| [1., pairs[i].benchmark],
        &residual,
        &breaks,
        effective_method(method, h),
    );
    let train_end = pairs[train_n - 1].end;
    let mut hs = crate::stable_hash::Fnv64::default();
    hs.write_u64(key);
    hs.write_u64(train_end as u64);
    hs.write_str(&format!("{:?}", p.transform));
    Ok(FairValueFit {
        fit_id: hs.finish(),
        transform: p.transform,
        train_end,
        n: train_n,
        alpha,
        beta,
        sigma,
        covariance,
        unit: unit_for(p.transform),
    })
}

/// Band multiples applied in target space, in output order.
pub const BAND_K: [f64; 4] = [-2., -1., 1., 2.];

/// Projection with σ added in target space before inversion. `x` is the target-space regressor
/// (X, ln X, ΔX or r^X); `anchor` is P_{t−h} for the Change / LogReturn transforms. Any non-finite
/// result gives `None`. The LogLevel centre is a conditional *median* (no σ²/2 term).
pub fn project(transform: Transform, a: f64, b: f64, x: f64, anchor: Option<f64>, sigma: f64) -> Option<(f64, [f64; 4])> {
    let at = |k: f64| -> Option<f64> {
        let target = a + b * x + k * sigma;
        finite(match transform {
            Transform::Level => target,
            Transform::LogLevel => target.exp(),
            Transform::Change { .. } => anchor? + target,
            Transform::LogReturn { .. } => anchor? * target.exp(),
        })
    };
    Some((at(0.)?, [at(BAND_K[0])?, at(BAND_K[1])?, at(BAND_K[2])?, at(BAND_K[3])?]))
}

/// Frozen-fit overlay over the latest ≤5,000 valid rows. Out-of-sample begins `h−1` bars after the
/// last training endpoint because overlapping returns share boundary prices.
pub fn fair_value(p: &Prepared, fit: &FairValueFit) -> Vec<FairValuePoint> {
    let h = if p.transform.is_level() { 1 } else { p.transform.horizon() };
    let train_row = p.ts.binary_search(&fit.train_end).unwrap_or(0);
    let mut out: Vec<FairValuePoint> = Vec::with_capacity(p.ts.len().min(MAX_POINTS + 1));
    let mut last = None;
    for t in 0..p.ts.len() {
        let (Some(y), Some(x)) = (p.y.values[t], p.x.values[t]) else { continue };
        let Some((centre, bands)) = project(fit.transform, fit.alpha, fit.beta, x, p.anchor[t], fit.sigma) else {
            continue;
        };
        out.push(FairValuePoint {
            ts: p.ts[t],
            centre,
            bands,
            break_before: p.y.break_before[t] || last != Some(t.wrapping_sub(1)),
            in_sample: t <= train_row + h - 1,
            x,
            y,
            price: p.price[t],
            anchor: p.anchor[t],
            z: (y - (fit.alpha + fit.beta * x)) / fit.sigma,
        });
        last = Some(t);
    }
    let cut = out.len().saturating_sub(MAX_POINTS);
    out.drain(..cut);
    if let Some(first) = out.first_mut() {
        first.break_before = true;
    }
    out
}

/// Hover numbers for one overlay point, all in price units: the mean-response CI
/// `ŷ ± 1.96·se_m` and the prediction interval with SE √(σ²+se_m²), each inverted like the bands.
#[derive(Debug, Clone, PartialEq)]
pub struct HoverStats {
    pub se_mean: f64,
    pub se_prediction: f64,
    pub mean_ci: [f64; 2],
    pub prediction_band: [f64; 2],
}
pub fn hover_stats(fit: &FairValueFit, p: &FairValuePoint) -> Option<HoverStats> {
    let se_mean = fit.covariance.mean_se(&[1., p.x])?;
    let se_prediction = (fit.sigma * fit.sigma + se_mean * se_mean).sqrt();
    let (_, mean) = project(fit.transform, fit.alpha, fit.beta, p.x, p.anchor, 1.96 * se_mean)?;
    let (_, pred) = project(fit.transform, fit.alpha, fit.beta, p.x, p.anchor, se_prediction)?;
    Some(HoverStats { se_mean, se_prediction, mean_ci: [mean[1], mean[2]], prediction_band: [pred[1], pred[2]] })
}

/// Expanding β by Welford co-moments over `(ts, y, x, break_before)`; a break resets the
/// accumulators. β needs n ≥ 3 and Cxx > n(16ε)². Returns the latest [`MAX_EXPANDING`] endpoints.
pub fn expanding_beta(points: &[(i64, f64, f64, bool)]) -> Vec<(i64, Option<f64>)> {
    let (mut n, mut mx, mut my, mut cxy, mut cxx) = (0usize, 0., 0., 0., 0.);
    let mut out = Vec::with_capacity(points.len());
    for &(ts, y, x, brk) in points {
        if brk {
            (n, mx, my, cxy, cxx) = (0, 0., 0., 0., 0.);
        }
        n += 1;
        let dx = x - mx;
        mx += dx / n as f64;
        my += (y - my) / n as f64;
        cxy += dx * (y - my);
        cxx += dx * (x - mx);
        let tolerance = n as f64 * (16. * f64::EPSILON).powi(2);
        out.push((ts, if n >= 3 && cxx > tolerance { finite(cxy / cxx) } else { None }));
    }
    let cut = out.len().saturating_sub(MAX_EXPANDING);
    out.drain(..cut);
    out
}

/// One complete-case set: timestamps present in every admitted row, where each return's
/// endpoints are consecutive in every row's own bars and no vault gap is spanned. Every cell uses
/// the same returns, so the same n. Pairwise `matrix()` stays the legacy conventions.
pub fn common_panel_matrix(rows: &[NamedBars], daily: bool) -> Matrix {
    let key = |ts: i64| if daily { ts.div_euclid(86_400) } else { ts };
    let errors: Vec<Option<String>> = rows
        .iter()
        .map(|r| {
            r.error
                .clone()
                .or_else(|| crate::stats::require_research_bars(&r.bars, "Correlation matrix").err())
                .or_else(|| {
                    (daily && r.bars.windows(2).any(|w| key(w[0].ts) == key(w[1].ts)))
                        .then(|| "two daily bars share one calendar date".into())
                })
        })
        .collect();
    let ok: Vec<usize> = (0..rows.len()).filter(|&i| errors[i].is_none()).collect();
    let mut gaps: Vec<(i64, i64)> = ok.iter().flat_map(|&i| rows[i].gaps.iter().copied()).collect();
    if daily {
        for g in &mut gaps {
            *g = (g.0.div_euclid(86_400) * 86_400 + 86_399, g.1.div_euclid(86_400) * 86_400);
        }
    }
    gaps.sort_unstable();
    gaps.dedup();
    // Common positions with each row's bar index.
    let keys: Vec<Vec<i64>> = ok.iter().map(|&i| rows[i].bars.iter().map(|b| key(b.ts)).collect()).collect();
    let mut at: Vec<Vec<usize>> = vec![Vec::new(); ok.len()];
    let mut common_ts = Vec::new();
    if let Some(first) = keys.first() {
        'k: for (i0, k) in first.iter().enumerate() {
            let mut found = Vec::with_capacity(ok.len());
            found.push(i0);
            for other in &keys[1..] {
                match other.binary_search(k) {
                    Ok(j) => found.push(j),
                    Err(_) => continue 'k,
                }
            }
            for (r, j) in found.into_iter().enumerate() {
                at[r].push(j);
            }
            common_ts.push(rows[ok[0]].bars[i0].ts);
        }
    }
    let valid: Vec<usize> = (1..common_ts.len())
        .filter(|&p| {
            at.iter().all(|a| a[p] == a[p - 1] + 1) && !crate::studio_dist::spans_gap(&gaps, common_ts[p - 1], common_ts[p])
        })
        .collect();
    let series: Vec<Vec<f64>> = ok
        .iter()
        .enumerate()
        .map(|(r, &i)| {
            let bars = &rows[i].bars;
            valid.iter().map(|&p| bars[at[r][p]].close.ln() - bars[at[r][p - 1]].close.ln()).collect()
        })
        .collect();
    let n = valid.len();
    let mut cells: Vec<Vec<MatrixCell>> = rows
        .iter()
        .enumerate()
        .map(|(i, _)| {
            (0..rows.len())
                .map(|j| {
                    let reason = errors[i].clone().or_else(|| errors[j].clone());
                    MatrixCell { correlation: None, n: 0, reason }
                })
                .collect()
        })
        .collect();
    for (a, &i) in ok.iter().enumerate() {
        for (b, &j) in ok.iter().enumerate().skip(a) {
            let pairs: Vec<PairReturn> = (0..n)
                .map(|k| PairReturn { start: 0, end: 1, asset: series[a][k], benchmark: series[b][k] })
                .collect();
            let (correlation, reason) = match regress(&pairs, 252.) {
                Ok(m) => (m.correlation, m.correlation.is_none().then(|| "Need ≥3 common returns and nonzero variance".to_string())),
                Err(e) => (None, Some(e)),
            };
            for (r, c) in [(i, j), (j, i)] {
                cells[r][c] = MatrixCell { correlation, n, reason: reason.clone() };
            }
        }
    }
    Matrix {
        symbols: rows.iter().map(|r| r.symbol.clone()).collect(),
        cells,
        unavailable: rows.iter().zip(errors).filter_map(|(r, e)| e.map(|e| (r.symbol.clone(), e))).collect(),
    }
}

fn tail_series(s: &ScalarSeries, n: usize) -> ScalarSeries {
    let from = s.ts.len().saturating_sub(n);
    let mut out = ScalarSeries {
        ts: s.ts[from..].to_vec(),
        values: s.values[from..].to_vec(),
        break_before: s.break_before[from..].to_vec(),
        unit: s.unit.clone(),
        receipt: s.receipt.clone(),
    };
    if let Some(b) = out.break_before.first_mut() {
        *b = true;
    }
    out
}

/// Everything Relate adds to a CORR report. Runs inside the existing worker blocking task.
#[allow(clippy::too_many_arguments)]
pub fn compute_relate(
    asset: &NamedBars,
    benchmark: &NamedBars,
    rows: &[NamedBars],
    options: Options,
    tf: Timeframe,
    spec: &RelateSpec,
    source: &str,
    closed_thru: i64,
    observed_at: Option<i64>,
    key: u64,
) -> Result<RelateReport, String> {
    let prepared = prepare(asset, benchmark, options, tf, spec.transform, source, closed_thru, observed_at)?;
    let mut unavailable = None;
    let fit = match training_fit(&prepared, key, spec.covariance) {
        Ok(f) => Some(f),
        Err(e) => {
            unavailable = Some(e);
            None
        }
    };
    let points = fit.as_ref().map(|f| fair_value(&prepared, f)).unwrap_or_default();
    let expanding = if spec.beta_mode == BetaMode::Expanding {
        let pts: Vec<_> = prepared
            .valid_rows()
            .into_iter()
            .map(|t| (prepared.ts[t], prepared.y.values[t].unwrap(), prepared.x.values[t].unwrap(), prepared.y.break_before[t]))
            .collect();
        expanding_beta(&pts)
    } else {
        Vec::new()
    };
    let matrix_common = (spec.matrix == MatrixMode::Common).then(|| common_panel_matrix(rows, options.daily));
    Ok(RelateReport {
        spec: spec.clone(),
        key,
        y: tail_series(&prepared.y, MAX_POINTS),
        x: tail_series(&prepared.x, MAX_POINTS),
        expanding,
        fit,
        points,
        matrix_common,
        unavailable,
        sources: [asset.source.clone(), benchmark.source.clone()],
        benchmark_gaps: benchmark.gaps.clone(),
    })
}

/// Parsed `transform= h= beta= bands= matrix=` words shared by `STUDIO lens=relate` and `RELATE`.
/// `h` is restricted to {1, 5, 20} (the horizons the overlap HAC kernel supports).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RelateArgs {
    pub transform: Option<String>,
    pub h: Option<usize>,
    pub beta: Option<BetaMode>,
    pub bands: Option<bool>,
    pub matrix: Option<MatrixMode>,
}
impl RelateArgs {
    pub fn parse(words: &[&str]) -> Result<Self, String> {
        let mut out = Self::default();
        for word in words {
            let (key, value) = word.split_once('=').ok_or_else(|| format!("unknown Relate input `{word}`"))?;
            let value = value.to_ascii_lowercase();
            match key.to_ascii_lowercase().as_str() {
                "transform" => {
                    Transform::parse(&value, 1)?;
                    out.transform = Some(value);
                }
                "h" => {
                    let h: usize = value.parse().map_err(|_| "Relate h must be 1, 5 or 20".to_string())?;
                    if ![1, 5, 20].contains(&h) {
                        return Err("Relate h must be 1, 5 or 20".into());
                    }
                    out.h = Some(h);
                }
                "beta" => {
                    out.beta = Some(match value.as_str() {
                        "rolling" => BetaMode::Rolling,
                        "expanding" => BetaMode::Expanding,
                        _ => return Err("beta must be rolling or expanding".into()),
                    })
                }
                "bands" => {
                    out.bands = Some(match value.as_str() {
                        "on" => true,
                        "off" => false,
                        _ => return Err("bands must be on or off".into()),
                    })
                }
                "matrix" => {
                    out.matrix = Some(match value.as_str() {
                        "pairwise" => MatrixMode::Pairwise,
                        "common" => MatrixMode::Common,
                        _ => return Err("matrix must be pairwise or common".into()),
                    })
                }
                _ => return Err(format!("unknown Relate input `{word}`")),
            }
        }
        Ok(out)
    }
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
    /// The transform these words select on top of `current` (a bare `h=` keeps the family).
    pub fn resolve(&self, current: Transform) -> Transform {
        let h = self.h.unwrap_or_else(|| current.horizon());
        match (&self.transform, current) {
            (Some(name), _) => Transform::parse(name, h).unwrap_or(current),
            (None, Transform::Change { .. }) if self.h.is_some() => Transform::Change { h },
            (None, Transform::LogReturn { .. }) if self.h.is_some() => Transform::LogReturn { h },
            (None, t) => t,
        }
    }
    /// Applies every word to a spec.
    pub fn apply(&self, spec: &mut RelateSpec) {
        spec.transform = self.resolve(spec.transform);
        if let Some(b) = self.beta {
            spec.beta_mode = b;
        }
        if let Some(b) = self.bands {
            spec.bands = b;
        }
        if let Some(m) = self.matrix {
            spec.matrix = m;
        }
    }
}
