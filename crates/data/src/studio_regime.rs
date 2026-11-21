//! Causal volatility-label distribution fingerprints; no fetching or iid KS inference.
use crate::{
    nulls::{Family, SplitMix64, rotation_test},
    studio_dist::{Report, population_sigma, quantile},
    studio_inference::{HacEstimate, hac_contrast_with},
};
use std::sync::Arc;
pub const RIDGE_BINS: usize = 48;
pub const CONTRAST_LAG: usize = 20;
pub const ROTATION_DRAWS: usize = 499;
pub const ROTATION_MIN_SHIFT: usize = 40;
pub const ROTATION_WORK: usize = 12_000_000;
pub const RUG: usize = 20;
pub const Q_PASS: f64 = 0.10;
pub const WILSON_Z: f64 = 1.96;
#[derive(Debug, Clone, PartialEq)]
pub struct Ridge {
    pub n: usize,
    pub complement_n: usize,
    pub counts: Vec<usize>,
    pub outside: [usize; 2],
    pub cdf: Vec<f64>,
    pub complement_cdf: Vec<f64>,
    pub quantiles: Option<[f64; 5]>,
    pub mean: Option<f64>,
    pub beyond: Option<[usize; 2]>,
    pub rug: [Vec<f64>; 2],
    pub contrast: HacEstimate,
    pub contrast_q: f64,
    pub up: Option<UpShare>,
    pub distance: Option<Distance>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct UpShare {
    pub k: usize,
    pub n: usize,
    pub share: f64,
    pub interval: (f64, f64),
}
#[derive(Debug, Clone, PartialEq)]
pub struct Distance {
    pub d: f64,
    pub at: f64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct RegimeFingerprints {
    pub slots: usize,
    pub classified: usize,
    pub warmup: usize,
    pub gap_excluded: usize,
    pub half_width: f64,
    pub pooled_mean: f64,
    pub pooled_sigma: f64,
    pub ridges: [Ridge; 3],
    pub rotation_input: Option<Arc<RotationInput>>,
    pub unavailable: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct RotationInput {
    pub seed: u64,
    pub labels: Vec<u8>,
    pub values: Vec<f64>,
    pub observed: [Option<f64>; 3],
}
#[derive(Debug, Clone, PartialEq)]
pub struct RegimeRotation {
    pub draws: usize,
    pub p: [Option<f64>; 3],
    pub q: [f64; 3],
    pub null_band: [Option<(f64, f64)>; 3],
    pub unavailable: Option<String>,
}
pub fn regime_slots(r: &Report) -> (Vec<Option<(usize, f64)>>, Vec<bool>) {
    let mut cursor = 0;
    let mut slots = Vec::with_capacity(r.underwater.len().saturating_sub(1));
    let mut breaks = Vec::with_capacity(slots.capacity());
    for pair in r.underwater.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let gap = crate::studio_dist::spans_gap(&r.gaps, a.ts, b.ts);
        while cursor < r.returns.len() && r.returns[cursor].ts < b.ts {
            cursor += 1;
        }
        slots.push(if !gap && cursor < r.returns.len() && r.returns[cursor].ts == b.ts {
            b.regime.map(|g| (g, r.returns[cursor].value))
        } else {
            None
        });
        breaks.push(gap);
    }
    (slots, breaks)
}
pub fn ridge_bins(values: &[f64], w: f64) -> (Vec<usize>, [usize; 2]) {
    let mut bins = vec![0; RIDGE_BINS];
    let mut outside = [0; 2];
    if !w.is_finite() || w <= 0. {
        return (bins, outside);
    }
    for &x in values {
        if x < -w {
            outside[0] += 1;
        } else if x > w {
            outside[1] += 1;
        } else {
            let k = (((x + w) / (2. * w) * RIDGE_BINS as f64).floor() as usize).min(RIDGE_BINS - 1);
            bins[k] += 1;
        }
    }
    (bins, outside)
}
/// Tie-complete, right-continuous ECDF sup distance. Smallest maximizer wins.
pub fn distance(a: &[f64], b: &[f64]) -> Option<Distance> {
    if a.is_empty() || b.is_empty() || a.iter().chain(b).any(|x| !x.is_finite()) {
        return None;
    }
    let mut pooled: Vec<_> = a.iter().map(|&x| (x, true)).chain(b.iter().map(|&x| (x, false))).collect();
    pooled.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut counts = [0; 2];
    let mut out = Distance { d: 0., at: pooled[0].0 };
    let mut i = 0;
    while i < pooled.len() {
        let x = pooled[i].0;
        while i < pooled.len() && pooled[i].0 == x {
            counts[usize::from(!pooled[i].1)] += 1;
            i += 1;
        }
        let d = (counts[0] as f64 / a.len() as f64 - counts[1] as f64 / b.len() as f64).abs();
        if d > out.d {
            out = Distance { d, at: x };
        }
    }
    Some(out)
}
pub fn wilson(k: usize, n: usize) -> Option<(f64, f64)> {
    if n == 0 || k > n {
        return None;
    }
    let (p, n, z) = (k as f64 / n as f64, n as f64, WILSON_Z);
    let denom = 1. + z * z / n;
    let centre = (p + z * z / (2. * n)) / denom;
    let half = z * (p * (1. - p) / n + z * z / (4. * n * n)).sqrt() / denom;
    Some(((centre - half).max(0.), (centre + half).min(1.)))
}
fn empty_ridge() -> Ridge {
    Ridge {
        n: 0,
        complement_n: 0,
        counts: vec![0; RIDGE_BINS],
        outside: [0; 2],
        cdf: vec![0.; RIDGE_BINS + 1],
        complement_cdf: vec![0.; RIDGE_BINS + 1],
        quantiles: None,
        mean: None,
        beyond: None,
        rug: [vec![], vec![]],
        contrast: hac_contrast_with(&[], CONTRAST_LAG, &[]),
        contrast_q: 1.,
        up: None,
        distance: None,
    }
}
fn cdf(bins: &[usize], outside: [usize; 2], n: usize) -> Vec<f64> {
    let mut sum = outside[0];
    std::iter::once(if n > 0 { sum as f64 / n as f64 } else { 0. })
        .chain(bins.iter().map(|b| {
            sum += b;
            if n > 0 { sum as f64 / n as f64 } else { 0. }
        }))
        .collect()
}
pub fn fingerprints(slots: &[Option<(usize, f64)>], breaks: &[bool], seed: u64) -> RegimeFingerprints {
    assert_eq!(slots.len(), breaks.len());
    let gap_excluded = breaks.iter().filter(|b| **b).count();
    let entries: Vec<_> = slots.iter().flatten().copied().collect();
    let mut pool: Vec<_> = entries.iter().map(|x| x.1).collect();
    pool.sort_by(f64::total_cmp);
    let mut out = RegimeFingerprints {
        slots: slots.len(),
        classified: pool.len(),
        warmup: slots.len() - pool.len() - gap_excluded,
        gap_excluded,
        half_width: 0.,
        pooled_mean: 0.,
        pooled_sigma: 0.,
        ridges: std::array::from_fn(|_| empty_ridge()),
        rotation_input: None,
        unavailable: None,
    };
    let reason = if pool.is_empty() {
        Some("needs 41 closed returns before the first point-in-time regime label")
    } else if entries.iter().any(|(g, x)| *g >= 3 || !x.is_finite()) {
        Some("invalid classified return")
    } else {
        None
    };
    if let Some(reason) = reason {
        out.unavailable = Some(reason.into());
        return out;
    }
    out.half_width = quantile(&pool, 0.005).abs().max(quantile(&pool, 0.995).abs());
    if out.half_width == 0. || !out.half_width.is_finite() || pool.first() == pool.last() {
        out.unavailable = Some("zero variance: every classified return is identical".into());
        return out;
    }
    out.pooled_mean = pool.iter().sum::<f64>() / pool.len() as f64;
    out.pooled_sigma = population_sigma(&pool, out.pooled_mean) * (pool.len() as f64 / (pool.len() - 1) as f64).sqrt();
    for g in 0..3 {
        let mut a: Vec<_> = entries.iter().filter(|x| x.0 == g).map(|x| x.1).collect();
        a.sort_by(f64::total_cmp);
        let b: Vec<_> = entries.iter().filter(|x| x.0 != g).map(|x| x.1).collect();
        let (counts, outside) = ridge_bins(&a, out.half_width);
        let (cb, co) = ridge_bins(&b, out.half_width);
        let n = a.len();
        let k = a.iter().filter(|x| **x > 0.).count();
        let grid: Vec<_> = slots.iter().map(|x| x.map(|(label, v)| (label == g, v))).collect();
        out.ridges[g] = Ridge {
            n,
            complement_n: b.len(),
            cdf: cdf(&counts, outside, n),
            complement_cdf: cdf(&cb, co, b.len()),
            counts,
            outside,
            quantiles: (!a.is_empty()).then(|| [0.05, 0.25, 0.5, 0.75, 0.95].map(|p| quantile(&a, p))),
            mean: (!a.is_empty()).then(|| a.iter().sum::<f64>() / n as f64),
            beyond: (!a.is_empty()).then(|| {
                [
                    a.iter().filter(|x| **x < out.pooled_mean - 2. * out.pooled_sigma).count(),
                    a.iter().filter(|x| **x > out.pooled_mean + 2. * out.pooled_sigma).count(),
                ]
            }),
            rug: [a.iter().take(RUG).copied().collect(), a.iter().rev().take(RUG).copied().collect()],
            contrast: hac_contrast_with(&grid, CONTRAST_LAG, breaks),
            contrast_q: 1.,
            up: wilson(k, n).map(|interval| UpShare { k, n, share: k as f64 / n as f64, interval }),
            distance: distance(&a, &b),
        };
    }
    let family = Family::by("dist.regime.mean", &out.ridges.each_ref().map(|r| r.contrast.p));
    for (r, q) in out.ridges.iter_mut().zip(family.q) {
        r.contrast_q = q;
    }
    out.rotation_input = Some(Arc::new(RotationInput {
        seed,
        labels: entries.iter().map(|e| e.0 as u8).collect(),
        values: entries.iter().map(|e| e.1).collect(),
        observed: out
            .ridges
            .each_ref()
            .map(|r| if r.n >= 30 && r.complement_n >= 30 { r.distance.as_ref().map(|d| d.d) } else { None }),
    }));
    out
}
impl RotationInput {
    pub fn compute(&self) -> RegimeRotation {
        let m = self.values.len();
        let mut out = RegimeRotation { draws: 0, p: [None; 3], q: [1.; 3], null_band: [None; 3], unavailable: None };
        if m != self.labels.len() || self.labels.iter().any(|g| *g >= 3) || self.values.iter().any(|x| !x.is_finite()) {
            out.unavailable = Some("invalid rotation input".into());
            return out;
        }
        let budget = ROTATION_DRAWS.min(ROTATION_WORK / m.max(1));
        if m < 2 * ROTATION_MIN_SHIFT || m - 2 * ROTATION_MIN_SHIFT + 1 < 99 || budget < 99 {
            out.unavailable =
                Some("needs at least 99 admissible shifts and work-budget draws (minimum shift 40)".into());
            return out;
        }
        let mut shifts: Vec<_> = (ROTATION_MIN_SHIFT..=m - ROTATION_MIN_SHIFT).collect();
        if shifts.len() > budget {
            let mut rng = SplitMix64(self.seed);
            for i in 0..budget {
                let j = i + rng.below(shifts.len() - i);
                shifts.swap(i, j);
            }
            shifts.truncate(budget);
        }
        let mut order: Vec<_> = (0..m).collect();
        order.sort_by(|&a, &b| self.values[a].total_cmp(&self.values[b]));
        let mut n = [0usize; 3];
        for &g in &self.labels {
            n[g as usize] += 1;
        }
        let inv = n.map(|n| if n > 0 { 1. / n as f64 } else { 0. });
        let cinv = n.map(|n| if m > n { 1. / (m - n) as f64 } else { 0. });
        let result = rotation_test(&self.observed, &shifts, |shift| {
            let mut count = [0usize; 3];
            let mut max = [0f64; 3];
            let mut i = 0;
            while i < m {
                let x = self.values[order[i]];
                loop {
                    let at = order[i] + shift;
                    let at = if at >= m { at - m } else { at };
                    count[self.labels[at] as usize] += 1;
                    i += 1;
                    if i == m || self.values[order[i]] != x {
                        break;
                    }
                }
                for g in 0..3 {
                    max[g] = max[g].max((count[g] as f64 * inv[g] - (i - count[g]) as f64 * cinv[g]).abs());
                }
            }
            (0..3).map(|g| self.observed[g].map(|_| max[g])).collect()
        });
        out.draws = shifts.len();
        for (g, r) in result.into_iter().enumerate() {
            out.p[g] = r.p;
            out.null_band[g] = r.band;
        }
        out.q = Family::by("dist.regime.distance", &out.p).q.try_into().unwrap();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-10, "{a} != {b}");
    }
    fn noise(rng: &mut SplitMix64) -> f64 {
        (rng.next() >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    }
    fn bars(n: usize) -> Vec<tr_core::Bar> {
        let mut rng = SplitMix64(42);
        let mut p = 100.;
        (0..n)
            .map(|i| {
                let o = p;
                p *= ((if (i / 100) % 2 == 0 { 0.002 } else { 0.03 }) * noise(&mut rng)).exp();
                tr_core::Bar::new(1_600_000_000 + i as i64 * 86400, o, o.max(p), o.min(p), p, 100.)
                    .with_provenance(tr_core::Provenance::Real)
            })
            .collect()
    }
    fn report(n: usize) -> Report {
        crate::studio_dist::compute(
            "AAPL",
            &bars(n),
            tr_core::Timeframe::D1,
            crate::studio_dist::Window::All,
            "unit",
            None,
        )
        .unwrap()
    }
    #[test]
    fn slots_join_by_timestamp_and_skip_gap_pairs() {
        let b = bars(120);
        let gap = (b[60].ts, b[61].ts);
        let r = crate::studio_dist::compute_with_gaps(
            "AAPL",
            &b,
            tr_core::Timeframe::D1,
            crate::studio_dist::Window::All,
            "unit",
            None,
            &[gap],
        )
        .unwrap();
        let (s, br) = regime_slots(&r);
        assert_eq!(s.len(), 119);
        assert_eq!(br.iter().filter(|x| **x).count(), 1);
        assert!(s[60].is_none());
        assert_eq!(s.iter().flatten().count(), r.regimes.iter().map(|r| r.n).sum::<usize>());
        // Removing an earlier return must not shift the labels of later timestamps.
        let mut changed = r.clone();
        changed.returns.remove(0);
        assert_eq!(regime_slots(&changed), (s, br));
    }
    #[test]
    fn complements_exclude_warmup_and_unclassified() {
        let s = [None, Some((0, 1.)), Some((1, 2.)), None, Some((2, 3.))];
        let f = fingerprints(&s, &[false; 5], 1);
        assert_eq!((f.classified, f.warmup), (3, 2));
        for r in f.ridges {
            assert_eq!((r.n, r.complement_n), (1, 2));
        }
    }
    #[test]
    fn ridge_bins_known_answer_and_mass_sums_to_one() {
        let (b, o) = ridge_bins(&[-3.25, -3., -0.0625, 0., 0.0625, 3., 3.5], 3.);
        assert_eq!((b[0], b[23], b[24], b[47], o), (1, 1, 2, 1, [1, 1]));
        assert_eq!(b.iter().sum::<usize>() + o.iter().sum::<usize>(), 7);
        for r in report(1200).regime.ridges {
            near((r.counts.iter().sum::<usize>() + r.outside.iter().sum::<usize>()) as f64 / r.n as f64, 1.);
        }
    }
    #[test]
    fn quantiles_type7_hand_vector() {
        for (p, e) in [0.05, 0.25, 0.5, 0.75, 0.95].into_iter().zip([1.2, 2., 3., 4., 4.8]) {
            near(quantile(&[1., 2., 3., 4., 5.], p), e);
        }
    }
    #[test]
    fn ks_distance_known_answer() {
        let d = distance(&[1., 2., 3., 4.], &[3., 4., 5., 6.]).unwrap();
        near(d.d, 0.5);
        near(d.at, 2.);
        near(distance(&[1., 1., 2.], &[1., 2., 2.]).unwrap().d, 1. / 3.);
        near(distance(&[1., 2., 3.], &[4., 5.]).unwrap().d, 1.);
        near(distance(&[1., 2.], &[1., 2.]).unwrap().d, 0.);
        let f = fingerprints(&(0..6).map(|i| Some((i / 2, (i + 1) as f64))).collect::<Vec<_>>(), &[false; 6], 1);
        for (r, d) in f.ridges.iter().zip([1., 0.5, 1.]) {
            near(r.distance.as_ref().unwrap().d, d);
        }
    }
    #[test]
    fn wilson_interval_hand_values() {
        for (k, lo, hi) in [(60, 0.50200, 0.69060), (50, 0.40383, 0.59617)] {
            let (a, b) = wilson(k, 100).unwrap();
            assert!((a - lo).abs() < 1e-4 && (b - hi).abs() < 1e-4);
        }
        let (a, b) = wilson(0, 10).unwrap();
        near(a, 0.);
        near(b, 3.8416 / 13.8416);
        assert!(wilson(0, 0).is_none());
    }
    #[test]
    fn mean_family_by_hand_vector_keeps_unavailable_slot() {
        let q = Family::by("dist.regime.mean", &[Some(0.01), Some(0.04), None]).q;
        for (a, b) in q.into_iter().zip([0.055, 0.11, 1.]) {
            near(a, b);
        }
    }
    #[test]
    fn rotation_known_answer_two_blocks() {
        let input = RotationInput {
            seed: 1,
            labels: (0..200).map(|i| u8::from(i >= 100)).collect(),
            values: (0..200).map(|i| i as f64).collect(),
            observed: [Some(1.), Some(1.), None],
        };
        let r = input.compute();
        assert_eq!(r.draws, 121);
        for g in 0..2 {
            near(r.p[g].unwrap(), 1. / 61.);
            near(r.q[g], 11. / 244.);
        }
        assert_eq!(r.p[2], None);
        near(r.q[2], 1.);
    }
    #[test]
    fn rotation_is_deterministic_for_same_seed_and_changes_with_seed() {
        let f = report(1200).regime;
        let a = f.rotation_input.unwrap();
        let r = a.compute();
        assert_eq!(r, a.compute());
        let mut b = (*a).clone();
        b.seed += 1;
        assert_ne!(r, b.compute());
    }
    #[test]
    fn rotation_p_is_uniformish_on_seeded_noise() {
        let mut rng = SplitMix64(42);
        let mut ps = vec![];
        for run in 0..120 {
            let labels: Vec<_> = (0..600).map(|i| ((i / 30) % 3) as u8).collect();
            let values: Vec<_> = (0..600).map(|_| noise(&mut rng)).collect();
            let a: Vec<_> = values.iter().zip(&labels).filter(|(_, g)| **g == 0).map(|(x, _)| *x).collect();
            let b: Vec<_> = values.iter().zip(&labels).filter(|(_, g)| **g != 0).map(|(x, _)| *x).collect();
            let input =
                RotationInput { seed: run, labels, values, observed: [Some(distance(&a, &b).unwrap().d), None, None] };
            ps.push(input.compute().p[0].unwrap());
        }
        let share = ps.iter().filter(|p| **p <= 0.10).count() as f64 / 120.;
        let mean = ps.iter().sum::<f64>() / 120.;
        assert!((0.02..=0.22).contains(&share), "{share}");
        assert!((0.38..=0.62).contains(&mean), "{mean}");
    }
    #[test]
    fn rotation_detects_vol_clustered_labels() {
        let f = report(3000).regime;
        let r = f.rotation_input.unwrap().compute();
        assert!(r.p[2].unwrap() <= 0.01, "{r:?}");
    }
    #[test]
    fn distance_p_withheld_without_enough_rotations() {
        let r = RotationInput {
            seed: 1,
            labels: (0..150).map(|i| u8::from(i >= 75)).collect(),
            values: (0..150).map(|i| i as f64).collect(),
            observed: [Some(1.), Some(1.), None],
        }
        .compute();
        assert_eq!(r.p, [None; 3]);
        assert!(r.unavailable.is_some());
    }
    #[test]
    fn sparse_and_flat_inputs_are_unavailable_not_zero() {
        assert!(fingerprints(&[None; 10], &[false; 10], 1).unavailable.is_some());
        assert!(fingerprints(&[Some((0, 1.)); 100], &[false; 100], 1).unavailable.is_some());
        assert!(fingerprints(&[Some((0, 0.)); 100], &[false; 100], 1).unavailable.is_some());
    }
    #[test]
    fn existing_labels_and_regime_counts_unchanged() {
        let r = report(1200);
        for g in 0..3 {
            assert_eq!(r.regime.ridges[g].n, r.regimes[g].n);
        }
    }
}
