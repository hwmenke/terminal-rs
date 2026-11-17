//! Asymptotic Bartlett HAC inference on an explicit, uncompressed session grid.
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
fn normal_p(z: f64) -> f64 {
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
        let runs: Vec<_> = breaks.iter().scan(0usize, |run, b| {
            *run += usize::from(*b);
            Some(*run)
        }).collect();
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
        let grid:Vec<_>=(0..60).map(|s| if s==20 {None} else {Some((s%7) as f64)}).collect();
        let mut breaks=vec![false;60];breaks[30]=true;
        let n=59.;let mean=grid.iter().flatten().sum::<f64>()/n;
        let scores:Vec<_>=grid.iter().map(|r|r.map(|r|(r-mean)/n).unwrap_or(0.)).collect();
        let mut v=0.;
        for s in 0..60 {
            v+=scores[s]*scores[s];
            for l in 1..=5.min(s) {if !(s-l<30 && s>=30) {v+=2.*(1.-l as f64/6.)*scores[s]*scores[s - l];}}
        }
        let result=hac_mean_with_breaks(&grid,&breaks);
        assert_eq!((result.n,result.span,result.lag),(59,60,5));
        assert!((result.estimate.unwrap()-mean).abs()<1e-14);
        assert!((result.variance.unwrap()-v).abs()<1e-14);
        assert_eq!(hac_mean_with_breaks(&grid,&vec![false;60]),hac_mean(&grid));
        assert_ne!(result.variance,hac_mean(&grid).variance);
    }
    #[test]
    fn exact_inference_threshold_and_zero_cells() {
        let grid:Vec<_>=(0..30).map(|s|Some((s%7) as f64)).collect();
        assert!(hac_mean(&grid[..29]).interval.is_none());
        assert!(hac_mean(&grid).interval.is_some());
        let zero=hac_mean_with_breaks(&[Some(0.);30],&[false;30]);
        assert_eq!(zero.estimate,Some(0.));
        assert!(zero.p.is_none() && zero.interval.is_none() && zero.unavailable.unwrap().contains("degenerate"));
    }
}
