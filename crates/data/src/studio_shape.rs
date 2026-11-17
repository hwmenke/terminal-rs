//! Shape of admitted closed returns. No loading or synthetic observations.
use crate::studio_dist::{Report, quantile};
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub seed: u64,
    pub horizon: usize,
    pub overlap: bool,
    pub values: Vec<f64>,
    pub breaks: Vec<bool>,
    pub ladder: [f64; 9],
    pub mean: f64,
    pub sd: f64,
    pub robust: f64,
    pub quantised: bool,
    pub bins: Vec<(f64, f64, usize)>,
    pub curves: Vec<[f64; 4]>,
    pub bandwidth: Option<f64>,
    pub student_df: Option<f64>,
    pub thresholds: Vec<Threshold>,
    pub years: Vec<Year>,
    pub tails: [f64; 8],
    pub river: Vec<[f64; 5]>,
    pub river_indices: Vec<usize>,
    pub timestamps: Vec<i64>,
    pub now: f64,
    pub percentile: f64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Threshold {
    pub level: f64,
    pub k: usize,
    pub probability: f64,
    pub interval: (f64, f64),
    pub normal: Option<f64>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Year {
    pub year: i32,
    pub n: usize,
    pub mean: f64,
    pub annual_vol: Option<f64>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Intervals {
    pub seed: u64,
    pub draws: usize,
    pub tails: [Option<(f64, f64)>; 8],
}
fn sorted(x: &[f64]) -> Vec<f64> {
    let mut x = x.to_vec();
    x.sort_by(f64::total_cmp);
    x
}
pub fn fd_bins(x: &[f64]) -> usize {
    let s = sorted(x);
    let width = 2. * (quantile(&s, 0.75) - quantile(&s, 0.25)) / (s.len() as f64).cbrt();
    if width > 0. { ((s[s.len() - 1] - s[0]) / width).ceil().clamp(12., 80.) as usize } else { 12 }
}
pub fn silverman(x: &[f64], sd: f64) -> Option<f64> {
    let s = sorted(x);
    let h = 0.9 * sd.min((quantile(&s, 0.75) - quantile(&s, 0.25)) / 1.34) * (x.len() as f64).powf(-0.2);
    (h > 0. && h.is_finite()).then_some(h)
}
pub fn tails(x: &[f64]) -> [f64; 8] {
    let s = sorted(x);
    std::array::from_fn(|k| {
        let upper = k >= 4;
        let p = if k % 4 < 2 { 0.05 } else { 0.01 };
        let q = quantile(&s, if upper { 1. - p } else { p });
        let v = if k % 2 == 0 {
            q
        } else {
            let t: Vec<_> = s.iter().copied().filter(|v| if upper { *v >= q } else { *v <= q }).collect();
            t.iter().sum::<f64>() / t.len() as f64
        };
        if upper { v } else { -v }
    })
}
pub fn compute(r: &Report, horizon: usize, overlap: bool) -> Result<Shape, String> {
    compute_with_thresholds(r, horizon, overlap, &[0.01, 0.02, 0.05])
}
pub fn compute_with_thresholds(r: &Report, horizon: usize, overlap: bool, levels: &[f64]) -> Result<Shape, String> {
    if levels.len() > 3 || levels.iter().any(|v| !v.is_finite() || *v < 0.) {
        return Err("Use up to three finite nonnegative thresholds".into());
    }

    r.provenance.require_verified("DIST Shape")?;
    if !(1..=250).contains(&horizon) {
        return Err("Shape horizon must be 1…250 bars".into());
    }
    let h = horizon;
    let mut values = Vec::new();
    let mut dates = Vec::new();
    let mut breaks = Vec::new();
    let mut run = Vec::new();
    // Keep bar slots: a missing return resets both horizon and rolling windows.
    let mut at = 0;
    let mut seam = true;
    for bar in r.underwater.iter().skip(1) {
        while at < r.returns.len() && r.returns[at].ts < bar.ts {
            at += 1;
        }
        if at >= r.returns.len() || r.returns[at].ts != bar.ts {
            run.clear();
            seam = true;
            continue;
        }
        run.push(r.returns[at].value);
        if run.len() >= h && (overlap || (run.len() % h == 0)) {
            values.push(run[run.len() - h..].iter().sum());
            dates.push(bar.ts);
            breaks.push(seam);
            seam = false;
        }
    }
    if values.len() < 4 {
        return Err("Shape needs at least four complete horizon returns".into());
    }
    let s = sorted(&values);
    let n = s.len();
    let mean = s.iter().sum::<f64>() / n as f64;
    let sd = (s.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt();
    let median = quantile(&s, 0.5);
    let quantised = s.iter().filter(|v| **v == 0.).count() as f64 / n as f64 > 0.3;
    let deviations: Vec<_> = s.iter().map(|v| (v - median).abs()).collect();
    let robust = if quantised {
        (std::f64::consts::PI / 2.).sqrt() * deviations.iter().sum::<f64>() / n as f64
    } else {
        1.4826 * quantile(&sorted(&deviations), 0.5)
    };
    let ladder = [0.01, 0.05, 0.1, 0.25, 0.5, 0.75, 0.9, 0.95, 0.99].map(|p| quantile(&s, p));
    let count = fd_bins(&s);
    let lo = s[0];
    let width = ((s[n - 1] - lo) / count as f64).max(1e-12);
    let mut bins: Vec<_> = (0..count).map(|i| (lo + i as f64 * width, lo + (i + 1) as f64 * width, 0)).collect();
    for x in &s {
        let k = (((x - lo) / width) as usize).min(count - 1);
        bins[k].2 += 1;
    }
    let bandwidth = silverman(&s, sd);
    let pop = (s.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64).sqrt();
    let excess = if pop > 0. { s.iter().map(|v| ((v - mean) / pop).powi(4)).sum::<f64>() / n as f64 - 3. } else { 0. };
    let df = 4. + 6. / excess;
    let scale = pop * ((df - 2.) / df).sqrt();
    let curves = (0..128)
        .map(|i| {
            let padding = bandwidth.unwrap_or(0.) * 4.;
            let x = lo - padding + i as f64 / 127. * (s[n - 1] - lo + 2. * padding);
            let kde = bandwidth.map_or(0., |b| {
                s.iter().map(|v| (-0.5 * ((x - v) / b).powi(2)).exp()).sum::<f64>()
                    / (n as f64 * b * (2. * std::f64::consts::PI).sqrt())
            });
            let normal = if sd > 0. {
                (-0.5 * ((x - mean) / sd).powi(2)).exp() / (sd * (2. * std::f64::consts::PI).sqrt())
            } else {
                0.
            };
            let t = if excess > 0. && scale > 0. {
                crate::studio_dist::student_density((x - mean) / scale, df) / scale
            } else {
                0.
            };
            [x, kde, normal, t]
        })
        .collect();
    let mut river = Vec::new();
    let mut river_indices = Vec::new();
    let mut start = 0;
    for i in 0..values.len() {
        if breaks[i] {
            start = i;
        }
        if i - start >= 250 {
            let w = sorted(&values[i - 250..i]);
            river.push([0.05, 0.25, 0.5, 0.75, 0.95].map(|p| quantile(&w, p)));
            river_indices.push(i);
        }
    }
    let thresholds = levels
        .iter()
        .map(|level| {
            let k = values.iter().filter(|x| x.abs() >= *level).count();
            let normal = if sd > 0. {
                let sf = |z: f64| {
                    let tail = crate::studio_inference::normal_p(z) / 2.;
                    if z >= 0. { tail } else { 1. - tail }
                };
                Some((sf((level - mean) / sd) + sf((level + mean) / sd)).clamp(0., 1.))
            } else {
                None
            };
            Threshold {
                level: *level,
                k,
                probability: k as f64 / n as f64,
                interval: crate::studio_regime::wilson(k, n).unwrap(),
                normal,
            }
        })
        .collect();
    let mut groups = std::collections::BTreeMap::<i32, Vec<f64>>::new();
    for (ts, x) in dates.iter().zip(&values) {
        use chrono::Datelike;
        if let Some(dt) = chrono::DateTime::from_timestamp(*ts, 0) {
            groups.entry(dt.year()).or_default().push(*x);
        }
    }
    let years = groups
        .into_iter()
        .map(|(year, x)| {
            let n = x.len();
            let mean = x.iter().sum::<f64>() / n as f64;
            let annual_vol = (n > 1).then(|| {
                (x.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64 * r.annual_periods / h as f64)
                    .sqrt()
            });
            Year { year, n, mean, annual_vol }
        })
        .collect();
    let now = *values.last().unwrap();
    let percentile = s.partition_point(|v| *v <= now) as f64 / n as f64;
    Ok(Shape {
        thresholds,
        years,
        seed: key_with_thresholds(r, h, overlap, levels),
        horizon: h,
        overlap,
        tails: tails(&values),
        values,
        breaks,
        ladder,
        mean,
        sd,
        robust,
        quantised,
        bins,
        curves,
        bandwidth,
        student_df: (excess > 0. && scale > 0.).then_some(df),
        river,
        river_indices,
        timestamps: dates,
        now,
        percentile,
    })
}
impl Shape {
    pub fn bootstrap(&self) -> Intervals {
        let mut samples: [Vec<f64>; 8] = std::array::from_fn(|_| Vec::new());
        crate::studio_inference::stationary_bootstrap(self.values.len(), 500, self.seed, &self.breaks, |indices| {
            let x: Vec<_> = indices.iter().map(|i| self.values[*i]).collect();
            for (i, v) in tails(&x).iter().enumerate() {
                samples[i].push(*v);
            }
        });
        Intervals {
            seed: self.seed,
            draws: 500,
            tails: std::array::from_fn(|i| {
                if (i % 4 >= 2 && self.values.len() < 100) || self.values.len() < 30 {
                    None
                } else {
                    let s = sorted(&samples[i]);
                    Some((quantile(&s, 0.05), quantile(&s, 0.95)))
                }
            }),
        }
    }
}

/// Closed-bars digest plus horizon/sampling, shared by cache and bootstrap.
pub fn key(r: &Report, h: usize, overlap: bool) -> u64 {
    let mut hash = crate::stable_hash::Fnv64::default();
    hash.write_u64(r.bars_seed);
    hash.write_str("dist.shape.v1");
    hash.write_u64(h as u64);
    hash.write_u64(overlap as u64);
    hash.finish()
}
#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::{Bar, Provenance, Timeframe};
    fn report(n: usize) -> Report {
        let mut price = 100.;
        let bars: Vec<_> = (0..=n)
            .map(|i| {
                price *= ((i as f64 * 1.71).sin() * 0.01).exp();
                Bar::new(1_600_000_000 + i as i64 * 86400, price, price, price, price, 1.)
                    .with_provenance(Provenance::Real)
            })
            .collect();
        crate::studio_dist::compute("AAPL", &bars, Timeframe::D1, crate::studio_dist::Window::All, "test", None)
            .unwrap()
    }
    #[test]
    fn fd_bins_known_answer() {
        let x: Vec<_> = (0..1000).map(|i| i as f64).collect();
        assert_eq!(fd_bins(&x), 12);
        let x: Vec<_> = (0..1000).map(|i| if i == 999 { 100000. } else { i as f64 }).collect();
        assert_eq!(fd_bins(&x), 80);
    }
    #[test]
    fn silverman_bandwidth_known_answer() {
        let x = [1., 2., 3., 4., 5.];
        let sd = 2.5_f64.sqrt();
        let expected = 0.9 * (2. / 1.34) * 5_f64.powf(-0.2);
        assert!((silverman(&x, sd).unwrap() - expected).abs() < 1e-12);
    }
    #[test]
    fn kde_integrates_to_one() {
        let s = compute(&report(1000), 1, false).unwrap();
        let h = s.bandwidth.unwrap();
        let lo = s.values.iter().copied().fold(f64::INFINITY, f64::min) - 8. * h;
        let hi = s.values.iter().copied().fold(f64::NEG_INFINITY, f64::max) + 8. * h;
        let step = (hi - lo) / 4096.;
        let integral = (0..4096)
            .map(|i| {
                let x = lo + (i as f64 + 0.5) * step;
                s.values.iter().map(|v| (-0.5 * ((x - v) / h).powi(2)).exp()).sum::<f64>()
                    / (s.values.len() as f64 * h * (2. * std::f64::consts::PI).sqrt())
                    * step
            })
            .sum::<f64>();
        assert!((integral - 1.).abs() < 1e-6);
        let grid_integral = s.curves.windows(2).map(|w| (w[1][0] - w[0][0]) * (w[0][1] + w[1][1]) / 2.).sum::<f64>();
        assert!((grid_integral - 1.).abs() < 0.01);
    }
    #[test]
    fn ladder_type7_hand_vector() {
        let x = [1., 2., 3., 4., 5.];
        let actual = [0.01, 0.05, 0.1, 0.25, 0.5, 0.75, 0.9, 0.95, 0.99].map(|p| quantile(&x, p));
        let expected = [1.04, 1.2, 1.4, 2., 3., 4., 4.6, 4.8, 4.96];
        for (a, b) in actual.iter().zip(expected) {
            assert!((a - b).abs() < 1e-12);
        }
    }
    #[test]
    fn mad_on_known_sample() {
        let x = [1., 2., 3., 4., 5.];
        let d: Vec<_> = x.iter().map(|v| f64::abs(v - 3.)).collect();
        assert_eq!(1.4826 * quantile(&sorted(&d), 0.5), 1.4826);
    }
    #[test]
    fn quantised_series_flags_and_stays_finite() {
        let mut r = report(400);
        for (i, x) in r.returns.iter_mut().enumerate() {
            x.value = if i % 4 == 0 { 0.01 } else { 0. };
        }
        let s = compute(&r, 1, false).unwrap();
        assert!(s.quantised && s.robust > 0. && s.sd.is_finite());
        assert!(s.curves.iter().flatten().all(|x| x.is_finite()));
        for x in &mut r.returns {
            x.value = 0.;
        }
        let flat = compute(&r, 1, false).unwrap();
        assert_eq!(flat.bandwidth, None);
        assert_eq!(flat.robust, 0.);
        assert!(flat.curves.iter().flatten().all(|x| x.is_finite()));
    }
    #[test]
    fn nonoverlap_h5_uses_every_fifth_close() {
        let r = report(101);
        let s = compute(&r, 5, false).unwrap();
        assert_eq!(s.values.len(), 20);
        for (i, x) in s.values.iter().enumerate() {
            assert!((x - r.returns[i * 5..i * 5 + 5].iter().map(|x| x.value).sum::<f64>()).abs() < 1e-12);
        }
        let o = compute(&r, 5, true).unwrap();
        assert_eq!(o.values.len(), 97);
    }
    #[test]
    fn es_at_least_var_both_tails() {
        let t = tails(&[-5., -3., -1., 0., 1., 2., 3., 10.]);
        for i in [0, 2, 4, 6] {
            assert!(t[i + 1] >= t[i]);
        }
        assert_eq!(t[0], 4.3);
        assert_eq!(t[1], 5.);
        assert!((t[4] - 7.55).abs() < 1e-12);
        assert_eq!(t[5], 10.);
    }
    #[test]
    fn bootstrap_ci_brackets_point_and_is_seeded() {
        let r = report(500);
        let s = compute(&r, 1, false).unwrap();
        let b = s.bootstrap();
        assert_eq!(b, s.bootstrap());
        assert_eq!(b.draws, 500);
        for (point, ci) in s.tails.iter().zip(b.tails) {
            let (lo, hi) = ci.unwrap();
            assert!(lo <= *point && *point <= hi, "point {point} outside [{lo},{hi}]");
        }
        assert!(b.tails.iter().all(|x| x.is_some_and(|(a, b)| a <= b && a.is_finite() && b.is_finite())));
        let mut other = s.clone();
        other.seed += 1;
        assert_ne!(b, other.bootstrap());
        let small = compute(&report(50), 1, false).unwrap().bootstrap();
        for i in [2, 3, 6, 7] {
            assert!(small.tails[i].is_none());
        }
    }
    #[test]
    fn rolling_bands_exclude_current_point() {
        let mut r = report(300);
        let a = compute(&r, 1, false).unwrap();
        r.returns[250].value = 100.;
        let b = compute(&r, 1, false).unwrap();
        assert_eq!(a.river[0], b.river[0]);
        assert_ne!(a.river[1], b.river[1]);
        assert_eq!(a.river.len(), 50);
    }
    #[test]
    fn gaps_reset_horizons_and_rolling_windows() {
        let mut r = report(600);
        r.returns.remove(299);
        let s = compute(&r, 5, false).unwrap();
        assert_eq!(s.values.len(), 119);
        assert_eq!(s.breaks.iter().filter(|b| **b).count(), 2);
        let s = compute(&r, 1, false).unwrap();
        assert_eq!(s.river.len(), 99);
    }
    #[test]
    fn threshold_probability_wilson_hand_values() {
        let mut r = report(100);
        for (i, x) in r.returns.iter_mut().enumerate() {
            x.value = if i < 60 { 0.02 } else { 0. };
        }
        let s = compute_with_thresholds(&r, 1, false, &[0.01]).unwrap();
        let t = &s.thresholds[0];
        assert_eq!(t.k, 60);
        assert_eq!(t.probability, 0.6);
        assert!((t.interval.0 - 0.50200).abs() < 1e-4);
        assert!((t.interval.1 - 0.69060).abs() < 1e-4);
        let s = compute_with_thresholds(&r, 1, false, &[0.]).unwrap();
        assert!((s.thresholds[0].normal.unwrap() - 1.).abs() < 1e-7);
        assert!(compute_with_thresholds(&r, 1, false, &[f64::NAN]).is_err());
    }
    #[test]
    fn year_strip_counts_match() {
        for h in [1, 5] {
            let s = compute(&report(1000), h, false).unwrap();
            assert_eq!(s.years.iter().map(|y| y.n).sum::<usize>(), s.values.len());
            let weighted = s.years.iter().map(|y| y.mean * y.n as f64).sum::<f64>() / s.values.len() as f64;
            assert!((weighted - s.mean).abs() < 1e-12);
            assert!(s.years.iter().all(|y| y.annual_vol.is_some_and(|v| v.is_finite())));
        }
    }
    #[test]
    fn key_tracks_bars_digest_and_parameters() {
        let r = report(400);
        let a = key(&r, 1, false);
        assert_ne!(a, key(&r, 5, false));
        assert_ne!(a, key(&r, 1, true));
        let mut r = r;
        r.bars_seed += 1;
        assert_ne!(a, key(&r, 1, false));
        r.provenance = Provenance::Sample;
        assert!(compute(&r, 1, false).is_err());
    }
}

pub fn key_with_thresholds(r: &Report, h: usize, overlap: bool, levels: &[f64]) -> u64 {
    let mut hash = crate::stable_hash::Fnv64::default();
    hash.write_u64(key(r, h, overlap));
    for level in levels {
        hash.write_f64_bits(*level);
    }
    hash.finish()
}
