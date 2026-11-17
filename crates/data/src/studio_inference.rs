//! Session-grid HAC means/contrasts, regression sandwiches and block inference.
//! Missing grid entries contribute zero score; they never shorten lag distance.
pub const MIN_N: usize = 30;

#[derive(Debug, Clone, PartialEq)]
pub struct HacEstimate {
    pub estimate: Option<f64>,
    pub n: usize,
    pub complement_n: usize,
    pub span: usize,
    pub lag: usize,
    pub variance: Option<f64>,
    pub se: Option<f64>,
    pub interval: Option<(f64, f64)>,
    pub p: Option<f64>,
    pub unavailable: Option<String>,
}

fn variance(scores: &[f64], lag: usize) -> f64 {
    let mut v = scores.iter().map(|u| u * u).sum::<f64>();
    for l in 1..=lag {
        let product = scores[l..].iter().zip(scores).map(|(a, b)| a * b).sum::<f64>();
        v += 2.0 * (1.0 - l as f64 / (lag + 1) as f64) * product;
    }
    v.max(0.0)
}

/// Two-sided standard normal tail, A&S 7.1.26 erfc approximation
/// (absolute error <1.5e-7). No infinite test statistic is constructed.
pub(crate) fn normal_p(z: f64) -> f64 {
    let x = z.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let poly = (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t;
    (poly * (-x * x).exp()).clamp(0.0, 1.0)
}

fn infer(grid: &[Option<(bool, f64)>], contrast: bool, breaks: &[bool]) -> HacEstimate {
    let lag = 5.min(grid.len().saturating_sub(1));
    let mut sums = [0.0; 2];
    let mut counts = [0usize; 2];
    let mut invalid = false;
    let mut magnitude = 0.0_f64;
    for &(g, r) in grid.iter().flatten() {
        invalid |= !r.is_finite();
        let i = usize::from(!g);
        sums[i] += r;
        counts[i] += 1;
        magnitude = magnitude.max(r.abs());
    }
    let means = [0, 1].map(|i| if counts[i] > 0 { sums[i] / counts[i] as f64 } else { 0.0 });
    let mut out = HacEstimate {
        estimate: (counts[0] > 0 && (!contrast || counts[1] > 0) && !invalid)
            .then_some(means[0] - if contrast { means[1] } else { 0.0 }),
        n: counts[0],
        complement_n: counts[1],
        span: grid.len(),
        lag,
        variance: None,
        se: None,
        interval: None,
        p: None,
        unavailable: None,
    };
    out.estimate = out.estimate.filter(|x| x.is_finite());
    let reason = if invalid || !sums.iter().all(|x| x.is_finite()) {
        Some("nonfinite input".to_string())
    } else if counts[0] < MIN_N || (contrast && counts[1] < MIN_N) {
        Some(format!("LOW N: need {MIN_N} observations {}", if contrast { "in each group" } else { "in the mean" }))
    } else if out.estimate.is_none() {
        Some("nonfinite contrast".into())
    } else {
        None
    };
    if let Some(reason) = reason {
        out.unavailable = Some(reason);
        return out;
    }
    let scores: Vec<f64> = grid
        .iter()
        .map(|v| match v {
            Some((g, r)) => {
                let i = usize::from(!g);
                (r - means[i]) / counts[i] as f64 * if *g { 1.0 } else { -1.0 }
            }
            None => 0.0,
        })
        .collect();
    let v = if breaks.is_empty() {
        variance(&scores, lag)
    } else {
        assert_eq!(breaks.len(), grid.len());
        let runs: Vec<_> = breaks
            .iter()
            .scan(0usize, |run, b| {
                *run += usize::from(*b);
                Some(*run)
            })
            .collect();
        let mut v = scores.iter().map(|u| u * u).sum::<f64>();
        for l in 1..=lag {
            for s in l..scores.len() {
                if runs[s] == runs[s - l] {
                    v += 2. * (1. - l as f64 / (lag + 1) as f64) * scores[s] * scores[s - l];
                }
            }
        }
        v.max(0.)
    };
    // Relative roundoff protection for constant returns. A zero SE does not
    // justify an infinite z or a p=0, even for a nonzero contrast.
    let se = v.sqrt();
    if !se.is_finite() || se <= 64.0 * f64::EPSILON * magnitude {
        out.unavailable = Some("zero or numerically degenerate HAC variance".into());
        return out;
    }
    let value = out.estimate.unwrap();
    let interval = (value - 1.96 * se, value + 1.96 * se);
    if !interval.0.is_finite() || !interval.1.is_finite() {
        out.unavailable = Some("nonfinite HAC interval".into());
        return out;
    }
    out.variance = Some(v);
    out.se = Some(se);
    out.interval = Some(interval);
    out.p = Some(normal_p(value / se));
    out
}

pub fn hac_mean(grid: &[Option<f64>]) -> HacEstimate {
    infer(&grid.iter().map(|r| r.map(|r| (true, r))).collect::<Vec<_>>(), false, &[])
}

/// Same mean and expected-session bandwidth as `hac_mean`, with lag products
/// reset at explicit vault discontinuities. `breaks[s]` starts a new run at s;
/// missing sessions remain on the original grid and do not create a break.
pub fn hac_mean_with_breaks(grid: &[Option<f64>], breaks: &[bool]) -> HacEstimate {
    assert_eq!(grid.len(), breaks.len());
    let boundaries = if breaks.iter().any(|b| *b) { breaks } else { &[] };
    infer(&grid.iter().map(|r| r.map(|r| (true, r))).collect::<Vec<_>>(), false, boundaries)
}

/// Each observed session belongs to the selected group (`true`) or its
/// same-clock/horizon complement (`false`). Unavailable sessions remain None.
pub fn hac_contrast(grid: &[Option<(bool, f64)>]) -> HacEstimate {
    infer(grid, true, &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hand_bartlett_variance_and_missing_lag_distance() {
        // sum u²=14; lag1=-8, lag2=3; weights 2/3, 1/3.
        assert!((variance(&[1., -2., 3.], 2) - 16. / 3.).abs() < 1e-12);
        assert!((variance(&[1., 0., -2., 0., 3.], 2) - 26. / 3.).abs() < 1e-12);
    }
    #[test]
    fn mean_and_contrast_match_direct_reference() {
        let grid: Vec<_> =
            (0..90)
                .map(|i| {
                    if i % 3 == 2 {
                        None
                    } else {
                        Some((i % 3 == 0, (i % 7) as f64 + if i % 3 == 0 { 2. } else { -1. }))
                    }
                })
                .collect();
        let a: Vec<_> = grid.iter().flatten().filter(|(g, _)| *g).map(|(_, r)| *r).collect();
        let b: Vec<_> = grid.iter().flatten().filter(|(g, _)| !*g).map(|(_, r)| *r).collect();
        let ma = a.iter().sum::<f64>() / 30.;
        let mb = b.iter().sum::<f64>() / 30.;
        let scores: Vec<_> = grid
            .iter()
            .map(|v| v.map(|(g, r)| if g { (r - ma) / 30. } else { -(r - mb) / 30. }).unwrap_or(0.))
            .collect();
        let mut reference = 0.;
        for s in 0..scores.len() {
            reference += scores[s].powi(2);
            for l in 1..=5.min(s) {
                reference += 2. * (1. - l as f64 / 6.) * scores[s] * scores[s - l];
            }
        }
        let r = hac_contrast(&grid);
        assert_eq!((r.n, r.complement_n, r.lag), (30, 30, 5));
        assert!((r.estimate.unwrap() - (ma - mb)).abs() < 1e-12);
        assert!((r.variance.unwrap() - reference).abs() < 1e-12);
        assert!((r.interval.unwrap().0 - (ma - mb - 1.96 * reference.sqrt())).abs() < 1e-12);
        let mean_grid: Vec<_> = grid.iter().map(|v| v.filter(|(g, _)| *g).map(|(_, r)| r)).collect();
        let mean = hac_mean(&mean_grid);
        assert!((mean.estimate.unwrap() - ma).abs() < 1e-12);
        let centered: Vec<_> = mean_grid.iter().map(|r| r.map(|r| (r - ma) / 30.).unwrap_or(0.)).collect();
        assert!((mean.variance.unwrap() - variance(&centered, 5)).abs() < 1e-12);
    }
    #[test]
    fn normal_tails_sparse_and_constant_are_honest() {
        assert!((normal_p(0.) - 1.).abs() < 1e-7);
        assert!((normal_p(1.96) - 0.04999579).abs() < 2e-7);
        assert!((normal_p(-3.) - 0.002699796).abs() < 2e-7);
        assert!(hac_mean(&[Some(1.); 29]).p.is_none());
        assert!(hac_mean(&[Some(1.); 30]).p.is_none());
        assert!(hac_mean(&[None; 30]).estimate.is_none());
        assert!(hac_mean(&[Some(f64::NAN); 30]).p.is_none());
        let r = hac_contrast(&(0..60).map(|i| Some((i < 30, if i < 30 { 1. } else { 2. }))).collect::<Vec<_>>());
        assert_eq!(r.estimate, Some(-1.));
        assert!(r.p.is_none() && r.interval.is_none());
    }
}

#[cfg(test)]
mod break_tests {
    use super::*;
    #[test]
    fn mean_join_reset_preserves_grid_bandwidth_and_known_variance() {
        let grid: Vec<_> = (0..60).map(|s| if s == 20 { None } else { Some((s % 7) as f64) }).collect();
        let mut breaks = vec![false; 60];
        breaks[30] = true;
        let n = 59.;
        let mean = grid.iter().flatten().sum::<f64>() / n;
        let scores: Vec<_> = grid.iter().map(|r| r.map(|r| (r - mean) / n).unwrap_or(0.)).collect();
        let mut v = 0.;
        for s in 0..60 {
            v += scores[s] * scores[s];
            for l in 1..=5.min(s) {
                if !(s - l < 30 && s >= 30) {
                    v += 2. * (1. - l as f64 / 6.) * scores[s] * scores[s - l];
                }
            }
        }
        let result = hac_mean_with_breaks(&grid, &breaks);
        assert_eq!((result.n, result.span, result.lag), (59, 60, 5));
        assert!((result.estimate.unwrap() - mean).abs() < 1e-14);
        assert!((result.variance.unwrap() - v).abs() < 1e-14);
        assert_eq!(hac_mean_with_breaks(&grid, &vec![false; 60]), hac_mean(&grid));
        assert_ne!(result.variance, hac_mean(&grid).variance);
    }
    #[test]
    fn exact_inference_threshold_and_zero_cells() {
        let grid: Vec<_> = (0..30).map(|s| Some((s % 7) as f64)).collect();
        assert!(hac_mean(&grid[..29]).interval.is_none());
        assert!(hac_mean(&grid).interval.is_some());
        let zero = hac_mean_with_breaks(&[Some(0.); 30], &[false; 30]);
        assert_eq!(zero.estimate, Some(0.));
        assert!(zero.p.is_none() && zero.interval.is_none() && zero.unavailable.unwrap().contains("degenerate"));
    }
}

/// Regression covariance choices. Bandwidth is part of the result, never implicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CovarianceMethod {
    Classical,
    HC3,
    HacFixed5,
    HacAutomatic,
    /// Uniform kernel for overlapping h-observation outcomes.
    Overlap(usize),
}
impl CovarianceMethod {
    pub fn label(self, lag: usize, fallback: bool) -> String {
        match self {
            Self::Classical => "Classical (iid, n−k)".into(),
            Self::HC3 => "HC3 (leverage adjusted)".into(),
            Self::HacFixed5 | Self::HacAutomatic => format!(
                "HAC Bartlett L={lag}{}",
                if self == Self::HacAutomatic { " (automatic)" } else { " (fixed 5)" }
            ),
            Self::Overlap(h) => format!(
                "{} L={lag}, h={h}",
                if fallback { "Bartlett fallback (uniform non-PSD)" } else { "Hansen–Hodrick uniform" }
            ),
        }
    }
}

pub fn automatic_bandwidth(n: usize) -> usize {
    (4. * (n as f64 / 100.).powf(2. / 9.)).floor() as usize
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RegressionCovariance {
    pub method: CovarianceMethod,
    pub lag: usize,
    pub n: usize,
    pub covariance: Option<Vec<Vec<f64>>>,
    pub fallback: bool,
    pub unavailable: Option<String>,
}
impl RegressionCovariance {
    pub fn label(&self) -> String {
        self.method.label(self.lag, self.fallback)
    }
    /// Mean response standard error, z V z'. No residual/prediction variance.
    pub fn mean_se(&self, z: &[f64]) -> Option<f64> {
        let v = self.covariance.as_ref()?;
        if z.len() != v.len() || z.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let variance: f64 =
            z.iter().enumerate().map(|(i, a)| z.iter().enumerate().map(|(j, b)| a * b * v[i][j]).sum::<f64>()).sum();
        (variance.is_finite() && variance >= 0.).then(|| variance.sqrt())
    }
}

fn inverse(a: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let k = a.len();
    let mut a = a.to_vec();
    let mut b = vec![vec![0.; k]; k];
    for (i, row) in b.iter_mut().enumerate() {
        row[i] = 1.;
    }
    for j in 0..k {
        let p = (j..k).max_by(|&i, &l| a[i][j].abs().total_cmp(&a[l][j].abs()))?;
        if a[p][j].abs() <= 1e-12 {
            return None;
        }
        a.swap(j, p);
        b.swap(j, p);
        let d = a[j][j];
        for l in 0..k {
            a[j][l] /= d;
            b[j][l] /= d;
        }
        for i in 0..k {
            if i != j {
                let d = a[i][j];
                for l in 0..k {
                    a[i][l] -= d * a[j][l];
                    b[i][l] -= d * b[j][l];
                }
            }
        }
    }
    Some(b)
}

/// Semidefinite LDL check, including off-diagonal entries in zero-pivot rows.
fn positive_semidefinite(a: &[Vec<f64>]) -> bool {
    let mut a = a.to_vec();
    let scale = a.iter().flatten().map(|x| x.abs()).fold(0., f64::max);
    if !scale.is_finite() || scale == 0. {
        return false;
    }
    let tol = scale * 1e-10;
    for j in 0..a.len() {
        if a[j][j] < -tol {
            return false;
        }
        if a[j][j] <= tol {
            if (j + 1..a.len()).any(|i| a[i][j].abs() > tol) {
                return false;
            }
        } else {
            for i in j + 1..a.len() {
                for l in i..a.len() {
                    a[l][i] -= a[i][j] * a[l][j] / a[j][j];
                    a[i][l] = a[l][i];
                }
            }
        }
    }
    true
}

/// Generic k-column sandwich at frozen OLS coefficients. `breaks[t]` begins a
/// new contiguous run. Missing elapsed observations must remain on the caller's
/// grid if that is the declared sampling convention. Columns are RMS-scaled for
/// numerical stability, then covariance is transformed back to original units.
pub fn regression_covariance(
    x: &[Vec<f64>],
    residual: &[f64],
    breaks: &[bool],
    method: CovarianceMethod,
) -> RegressionCovariance {
    let n = x.len();
    let k = x.first().map_or(0, Vec::len);
    let lag = match method {
        CovarianceMethod::HacFixed5 => 5,
        CovarianceMethod::HacAutomatic => automatic_bandwidth(n),
        CovarianceMethod::Overlap(h) => h.saturating_sub(1),
        _ => 0,
    }
    .min(n.saturating_sub(k + 1));
    let mut out = RegressionCovariance { method, lag, n, covariance: None, fallback: false, unavailable: None };
    if k == 2
        && n >= MIN_N
        && residual.len() == n
        && breaks.len() == n
        && x.iter().all(|r| r.len() == 2 && r.iter().all(|v| v.is_finite()))
        && residual.iter().all(|e| e.is_finite())
    {
        return covariance_two(x, residual, breaks, out);
    }

    let result = (|| -> Result<Vec<Vec<f64>>, String> {
        if n < MIN_N || k == 0 || n <= k {
            return Err("LOW N: covariance requires n≥30 and n>k".into());
        }
        if residual.len() != n
            || breaks.len() != n
            || x.iter().any(|r| r.len() != k || r.iter().any(|v| !v.is_finite()))
            || residual.iter().any(|v| !v.is_finite())
        {
            return Err("invalid regression input".into());
        }
        let scale: Vec<_> = (0..k).map(|j| (x.iter().map(|r| r[j] * r[j]).sum::<f64>() / n as f64).sqrt()).collect();
        if scale.iter().any(|s| !s.is_finite() || *s == 0.) {
            return Err("singular design".into());
        }
        let x: Vec<Vec<f64>> = x.iter().map(|r| r.iter().zip(&scale).map(|(v, s)| v / s).collect()).collect();
        let mut a = vec![vec![0.; k]; k];
        for r in &x {
            for i in 0..k {
                for j in 0..k {
                    a[i][j] += r[i] * r[j];
                }
            }
        }
        let inv = inverse(&a).ok_or("singular or ill-conditioned design")?;
        let sse = residual.iter().map(|e| e * e).sum::<f64>();
        if !sse.is_finite() || sse == 0. {
            return Err("zero or nonfinite residual variance".into());
        }
        if method == CovarianceMethod::Classical {
            return Ok((0..k)
                .map(|i| (0..k).map(|j| inv[i][j] * sse / (n - k) as f64 / scale[i] / scale[j]).collect())
                .collect());
        }
        let mut scores = vec![vec![0.; k]; n];
        for t in 0..n {
            let h: f64 = (0..k).map(|i| (0..k).map(|j| x[t][i] * inv[i][j] * x[t][j]).sum::<f64>()).sum();
            let divisor = if method == CovarianceMethod::HC3 { 1. - h } else { 1. };
            if divisor <= 1e-10 {
                return Err("unit leverage: HC3 unavailable".into());
            }
            for i in 0..k {
                scores[t][i] = x[t][i] * residual[t] / divisor;
            }
        }
        let runs: Vec<_> = breaks
            .iter()
            .scan(0usize, |r, b| {
                *r += usize::from(*b);
                Some(*r)
            })
            .collect();
        let meat = |bartlett: bool| {
            let mut s = vec![vec![0.; k]; k];
            for t in 0..n {
                for i in 0..k {
                    for j in 0..k {
                        s[i][j] += scores[t][i] * scores[t][j];
                    }
                }
            }
            for l in 1..=lag {
                let w = if bartlett { 1. - l as f64 / (lag + 1) as f64 } else { 1. };
                for t in l..n {
                    if runs[t] == runs[t - l] {
                        for i in 0..k {
                            for j in 0..k {
                                s[i][j] += w * (scores[t][i] * scores[t - l][j] + scores[t - l][i] * scores[t][j]);
                            }
                        }
                    }
                }
            }
            s
        };
        let uniform = matches!(method, CovarianceMethod::Overlap(_));
        let mut s = meat(!uniform);
        if uniform && (!positive_semidefinite(&s) || (0..k).any(|i| s[i][i] <= 0.)) {
            out.fallback = true;
            s = meat(true);
        }
        let mut v = vec![vec![0.; k]; k];
        for i in 0..k {
            for j in 0..k {
                for p in 0..k {
                    for q in 0..k {
                        v[i][j] += inv[i][p] * s[p][q] * inv[q][j] / scale[i] / scale[j];
                    }
                }
            }
        }
        if !v.iter().flatten().all(|x| x.is_finite()) || !positive_semidefinite(&v) {
            return Err("nonfinite or non-PSD covariance".into());
        }
        Ok(v)
    })();
    match result {
        Ok(v) => out.covariance = Some(v),
        Err(e) => out.unavailable = Some(e),
    }
    out
}

// Allocation-bounded two-column path used by the rolling relationship windows.
// Same sandwich as the generic k-column implementation above.
fn covariance_two(x: &[Vec<f64>], e: &[f64], breaks: &[bool], mut out: RegressionCovariance) -> RegressionCovariance {
    let result = (|| -> Result<Vec<Vec<f64>>, String> {
        let n = x.len();
        let scales = [
            (x.iter().map(|r| r[0] * r[0]).sum::<f64>() / n as f64).sqrt(),
            (x.iter().map(|r| r[1] * r[1]).sum::<f64>() / n as f64).sqrt(),
        ];
        if scales.iter().any(|s| !s.is_finite() || *s == 0.) {
            return Err("singular design".into());
        }
        let mut a = [0.; 3];
        for r in x {
            let z = [r[0] / scales[0], r[1] / scales[1]];
            a[0] += z[0] * z[0];
            a[1] += z[0] * z[1];
            a[2] += z[1] * z[1];
        }
        let det = a[0] * a[2] - a[1] * a[1];
        if !det.is_finite() || det <= 1e-12 * a[0] * a[2] {
            return Err("singular or ill-conditioned design".into());
        }
        let inv = [a[2] / det, -a[1] / det, a[0] / det];
        let sse = e.iter().map(|v| v * v).sum::<f64>();
        if sse == 0. || !sse.is_finite() {
            return Err("zero or nonfinite residual variance".into());
        }
        if out.method == CovarianceMethod::Classical {
            let s = sse / (n - 2) as f64;
            return Ok(vec![
                vec![inv[0] * s / scales[0].powi(2), inv[1] * s / scales[0] / scales[1]],
                vec![inv[1] * s / scales[0] / scales[1], inv[2] * s / scales[1].powi(2)],
            ]);
        }
        let mut scores = Vec::with_capacity(n);
        let mut runs = Vec::with_capacity(n);
        let mut run = 0;
        for t in 0..n {
            run += usize::from(breaks[t]);
            runs.push(run);
            let z = [x[t][0] / scales[0], x[t][1] / scales[1]];
            let d = if out.method == CovarianceMethod::HC3 {
                1. - z[0] * z[0] * inv[0] - 2. * z[0] * z[1] * inv[1] - z[1] * z[1] * inv[2]
            } else {
                1.
            };
            if d <= 1e-10 {
                return Err("unit leverage: HC3 unavailable".into());
            }
            scores.push([z[0] * e[t] / d, z[1] * e[t] / d]);
        }
        let meat = |bartlett: bool| {
            let mut s = [0.; 3];
            for g in &scores {
                s[0] += g[0] * g[0];
                s[1] += g[0] * g[1];
                s[2] += g[1] * g[1];
            }
            for l in 1..=out.lag {
                let w = if bartlett { 1. - l as f64 / (out.lag + 1) as f64 } else { 1. };
                for t in l..n {
                    if runs[t] == runs[t - l] {
                        let (g, h) = (scores[t], scores[t - l]);
                        s[0] += 2. * w * g[0] * h[0];
                        s[1] += w * (g[0] * h[1] + h[0] * g[1]);
                        s[2] += 2. * w * g[1] * h[1];
                    }
                }
            }
            s
        };
        let psd = |s: [f64; 3]| s[0] >= 0. && s[2] >= 0. && s[0] * s[2] - s[1] * s[1] >= -1e-10 * (s[0] * s[2]).abs();
        let uniform = matches!(out.method, CovarianceMethod::Overlap(_));
        let mut s = meat(!uniform);
        if uniform && (!psd(s) || s[0] <= 0. || s[2] <= 0.) {
            s = meat(true);
            out.fallback = true;
        }
        let v00 = inv[0] * inv[0] * s[0] + 2. * inv[0] * inv[1] * s[1] + inv[1] * inv[1] * s[2];
        let v01 = inv[0] * inv[1] * s[0] + (inv[0] * inv[2] + inv[1] * inv[1]) * s[1] + inv[1] * inv[2] * s[2];
        let v11 = inv[1] * inv[1] * s[0] + 2. * inv[1] * inv[2] * s[1] + inv[2] * inv[2] * s[2];
        if !psd([v00, v01, v11]) || ![v00, v01, v11].iter().all(|v| v.is_finite()) {
            return Err("nonfinite or non-PSD covariance".into());
        }
        Ok(vec![
            vec![v00 / scales[0].powi(2), v01 / scales[0] / scales[1]],
            vec![v01 / scales[0] / scales[1], v11 / scales[1].powi(2)],
        ])
    })();
    match result {
        Ok(v) => out.covariance = Some(v),
        Err(e) => out.unavailable = Some(e),
    };
    out
}

/// Stationary bootstrap indices with geometric blocks of mean ceil(n^(1/3)).
/// A continuation never crosses an explicit join. Restarts sample the full grid,
/// preserving missing cells when the caller applies the indices to whole rows.
pub fn stationary_bootstrap(n: usize, draws: usize, seed: u64, breaks: &[bool], mut consume: impl FnMut(&[usize])) {
    assert_eq!(n, breaks.len());
    if n == 0 {
        return;
    }
    let block = (n as f64).cbrt().ceil() as usize;
    let mut rng = crate::nulls::SplitMix64(seed);
    let separated = breaks.iter().any(|b| *b);
    let mut indices = vec![0; n];
    for _ in 0..draws {
        let mut at = rng.below(n);
        for (t, index) in indices.iter_mut().enumerate() {
            if t > 0 {
                let next = (at + 1) % n;
                if breaks[next] || (next == 0 && separated) || rng.below(block) == 0 {
                    at = rng.below(n);
                } else {
                    at = next;
                }
            }
            *index = at;
        }
        consume(&indices);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaxTBand {
    pub intervals: Vec<Option<(f64, f64)>>,
    pub critical: Option<f64>,
    pub draws: usize,
    pub block: usize,
    pub unavailable: Option<String>,
}
/// Session-row stationary bootstrap, common draws for every predeclared clock.
/// Fixed original HAC studentizers: max |(mean*−mean)/SE_HAC|. Missing resampled
/// cells stay missing. Only clocks with valid original inference enter the max;
/// intervals for other clocks remain unavailable. No per-clock selection by p.
pub fn session_max_t(columns: &[Vec<Option<f64>>], breaks: &[bool], draws: usize, seed: u64) -> MaxTBand {
    session_max_t_with_clock_breaks(columns, &vec![breaks.to_vec(); columns.len()], breaks, draws, seed)
}
/// Clock-specific original HAC breaks, with a common union of session breaks
/// for row resampling. Used when a vault join falls inside a cash session.
pub fn session_max_t_with_clock_breaks(
    columns: &[Vec<Option<f64>>],
    clock_breaks: &[Vec<bool>],
    breaks: &[bool],
    draws: usize,
    seed: u64,
) -> MaxTBand {
    let n = breaks.len();
    let mut out = MaxTBand {
        intervals: vec![None; columns.len()],
        critical: None,
        draws,
        block: (n as f64).cbrt().ceil() as usize,
        unavailable: None,
    };
    if draws < 99 || columns.is_empty() || columns.iter().any(|c| c.len() != n) {
        out.unavailable = Some("need ≥99 draws and a rectangular session grid".into());
        return out;
    }
    if clock_breaks.len() != columns.len() || clock_breaks.iter().any(|b| b.len() != n) {
        out.unavailable = Some("invalid clock break grid".into());
        return out;
    }
    let estimates: Vec<_> = columns.iter().zip(clock_breaks).map(|(c, b)| hac_mean_with_breaks(c, b)).collect();
    let eligible: Vec<_> = estimates.iter().enumerate().filter_map(|(k, e)| Some((k, e.estimate?, e.se?))).collect();
    if eligible.is_empty() {
        out.unavailable = Some("no clocks with available HAC inference".into());
        return out;
    }
    let mut maxima = Vec::with_capacity(draws);
    stationary_bootstrap(n, draws, seed, breaks, |indices| {
        let mut max = 0.0_f64;
        let mut valid = true;
        for &(k, mean, se) in &eligible {
            let (mut sum, mut count) = (0., 0usize);
            for &s in indices {
                if let Some(v) = columns[k][s] {
                    sum += v;
                    count += 1;
                }
            }
            if count < MIN_N {
                valid = false;
                break;
            }
            max = max.max(((sum / count as f64 - mean) / se).abs());
        }
        if valid && max.is_finite() {
            maxima.push(max);
        }
    });
    // Never discard failed replicates and silently change the bootstrap law.
    if maxima.len() != draws {
        out.unavailable = Some("a bootstrap replicate has fewer than 30 observations in an eligible clock".into());
        return out;
    }
    maxima.sort_by(f64::total_cmp);
    let critical = maxima[((0.95 * (draws + 1) as f64).ceil() as usize).saturating_sub(1).min(draws - 1)];
    out.critical = Some(critical);
    for (k, mean, se) in eligible {
        out.intervals[k] = Some((mean - critical * se, mean + critical * se));
    }
    out
}

#[cfg(test)]
mod v2_tests {
    use super::*;
    fn design() -> (Vec<Vec<f64>>, Vec<f64>) {
        (
            (0..30).map(|i| vec![1., (i % 3) as f64 - 1.]).collect(),
            (0..30).map(|i| if i % 3 == 1 { -2. } else { 1. }).collect(),
        )
    }
    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-11, "{a} != {b}");
    }
    #[test]
    fn hc3_matches_hand_vector() {
        let (x, e) = design();
        let r = regression_covariance(&x, &e, &[false; 30], CovarianceMethod::HC3);
        let v = r.covariance.unwrap();
        near(v[0][0], (20. / (11.0_f64 / 12.).powi(2) + 40. / (29.0_f64 / 30.).powi(2)) / 900.);
        near(v[1][1], 20. / (11.0_f64 / 12.).powi(2) / 400.);
        near(v[0][1], 0.);
    }
    #[test]
    fn hac_matches_direct_2x2_reference() {
        let x: Vec<_> = (0..75).map(|i| vec![1., (i % 11) as f64 * 0.001 - 0.004]).collect();
        let e: Vec<_> = (0..75).map(|i| ((i % 7) as f64 - 3.) * 0.0001).collect();
        let mut breaks = vec![false; 75];
        breaks[34] = true;
        let r = regression_covariance(&x, &e, &breaks, CovarianceMethod::HacFixed5);
        let mut a = [[0.; 2]; 2];
        let mut s = [[0.; 2]; 2];
        for t in 0..75 {
            for i in 0..2 {
                for j in 0..2 {
                    a[i][j] += x[t][i] * x[t][j];
                    s[i][j] += x[t][i] * x[t][j] * e[t] * e[t];
                    for l in 1..=5.min(t) {
                        if !(t - l < 34 && t >= 34) {
                            s[i][j] += (1. - l as f64 / 6.)
                                * e[t]
                                * e[t - l]
                                * (x[t][i] * x[t - l][j] + x[t - l][i] * x[t][j]);
                        }
                    }
                }
            }
        }
        let det = a[0][0] * a[1][1] - a[0][1] * a[1][0];
        let inv = [[a[1][1] / det, -a[0][1] / det], [-a[1][0] / det, a[0][0] / det]];
        let v = r.covariance.unwrap();
        for i in 0..2 {
            for j in 0..2 {
                let mut reference = 0.;
                for p in 0..2 {
                    for q in 0..2 {
                        reference += inv[i][p] * s[p][q] * inv[q][j];
                    }
                }
                near(v[i][j], reference);
            }
        }
        assert_eq!(r.lag, 5);
        assert_eq!(automatic_bandwidth(5000), 9);
    }
    #[test]
    fn hansen_hodrick_h3_known_answer() {
        // 10 independent [1,0,0] score impulses: sum e²=10, lag1=lag2=0.
        let x = vec![vec![1.]; 30];
        let e: Vec<_> = (0..30).map(|i| if i % 3 == 0 { 1. } else { 0. }).collect();
        let r = regression_covariance(&x, &e, &[false; 30], CovarianceMethod::Overlap(3));
        assert_eq!(r.lag, 2);
        assert!(!r.fallback);
        near(r.covariance.unwrap()[0][0], 10. / 900.);
    }
    #[test]
    fn nonpsd_falls_back_and_labels() {
        let x = vec![vec![1.]; 30];
        let e: Vec<_> = (0..30).map(|i| if i % 2 == 0 { 1. } else { -1. }).collect();
        let r = regression_covariance(&x, &e, &[false; 30], CovarianceMethod::Overlap(2));
        assert!(r.fallback && r.label().contains("Bartlett fallback"));
        near(r.covariance.unwrap()[0][0], 1. / 900.);
    }
    #[test]
    fn fan_width_at_zero_and_nonzero_x() {
        let r = RegressionCovariance {
            method: CovarianceMethod::HC3,
            lag: 0,
            n: 50,
            covariance: Some(vec![vec![4., 1.], vec![1., 9.]]),
            fallback: false,
            unavailable: None,
        };
        near(r.mean_se(&[1., 0.]).unwrap(), 2.);
        near(r.mean_se(&[1., 2.]).unwrap(), 44.0_f64.sqrt());
        assert!(r.mean_se(&[f64::NAN, 1.]).is_none());
    }
    #[test]
    fn no_lag_product_across_vault_gap_and_k_regressors() {
        let x = vec![vec![1.]; 60];
        let e: Vec<_> = (0..60).map(|i| (i % 7) as f64 - 3.).collect();
        let mut breaks = vec![false; 60];
        breaks[30] = true;
        let r = regression_covariance(&x, &e, &breaks, CovarianceMethod::HacFixed5).covariance.unwrap();
        let mut reference = e.iter().map(|e| e * e).sum::<f64>();
        for t in 0..60 {
            for l in 1..=5.min(t) {
                if !(t - l < 30 && t >= 30) {
                    reference += 2. * (1. - l as f64 / 6.) * e[t] * e[t - l];
                }
            }
        }
        near(r[0][0], reference / 3600.);
        assert_ne!(r, regression_covariance(&x, &e, &[false; 60], CovarianceMethod::HacFixed5).covariance.unwrap());
        let x: Vec<_> = (0..90).map(|i| vec![1., (i % 7) as f64, (i % 11) as f64]).collect();
        assert_eq!(
            regression_covariance(&x, &vec![1.; 90], &vec![false; 90], CovarianceMethod::HC3).covariance.unwrap().len(),
            3
        );
    }
    #[test]
    fn sparse_singular_flat_and_unit_leverage_are_unavailable() {
        for method in [CovarianceMethod::HC3, CovarianceMethod::HacFixed5, CovarianceMethod::HacAutomatic] {
            assert!(
                regression_covariance(&vec![vec![1.]; 29], &vec![1.; 29], &vec![false; 29], method)
                    .covariance
                    .is_none()
            );
            assert!(
                regression_covariance(&vec![vec![1., 1.]; 30], &vec![1.; 30], &vec![false; 30], method)
                    .covariance
                    .is_none()
            );
            assert!(
                regression_covariance(&vec![vec![1.]; 30], &vec![0.; 30], &vec![false; 30], method)
                    .covariance
                    .is_none()
            );
        }
        let x: Vec<_> = (0..30).map(|i| vec![1., if i == 0 { 1. } else { 0. }]).collect();
        assert!(
            regression_covariance(&x, &[1.; 30], &[false; 30], CovarianceMethod::HC3)
                .unavailable
                .unwrap()
                .contains("leverage")
        );
    }
    #[test]
    fn bootstrap_is_deterministic_for_same_digest() {
        let mut bars: Vec<_> = (0..60)
            .map(|i| tr_core::Bar::new(i, 100., 101., 99., 100., 1.).with_provenance(tr_core::Provenance::Real))
            .collect();
        let digest = crate::stable_hash::bars_digest(&bars);
        bars[20].close += 0.01;
        let changed = crate::stable_hash::bars_digest(&bars);
        let mut draws = Vec::new();
        stationary_bootstrap(60, 5, digest, &[false; 60], |i| draws.push(i.to_vec()));
        let mut second = Vec::new();
        stationary_bootstrap(60, 5, digest, &[false; 60], |i| second.push(i.to_vec()));
        assert_eq!(draws, second);
        let mut other = Vec::new();
        stationary_bootstrap(60, 5, changed, &[false; 60], |i| other.push(i.to_vec()));
        assert_ne!(draws, other);
        assert!(draws.iter().flatten().all(|i| *i < 60));
        // Every location is a boundary: no continuation, reproducibly iid starts.
        let mut separated = Vec::new();
        stationary_bootstrap(60, 5, digest, &[true; 60], |i| separated.push(i.to_vec()));
        assert_ne!(draws, separated);
    }
    #[test]
    fn maxt_common_rows_missing_cells_and_unavailable_stay_unavailable() {
        let c: Vec<_> = (0..100).map(|s| Some((s % 7) as f64)).collect();
        let columns = vec![c.clone(), c, vec![None; 100], vec![Some(1.); 100]];
        let a = session_max_t(&columns, &[false; 100], 500, 123);
        assert_eq!(a, session_max_t(&columns, &[false; 100], 500, 123));
        assert!(a.critical.unwrap() > 0.);
        assert_eq!(a.intervals[0], a.intervals[1]);
        assert!(a.intervals[2].is_none() && a.intervals[3].is_none());
        assert!(session_max_t(&columns, &[false; 100], 98, 123).critical.is_none());
    }
}
