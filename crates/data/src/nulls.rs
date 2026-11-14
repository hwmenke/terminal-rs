//! Deterministic randomization tests and descriptive multiple-test adjustment.

/// Benjamini–Hochberg step-up q values, returned in the input order.
pub fn benjamini_hochberg(p: &[f64]) -> Vec<f64> {
    assert!(p.iter().all(|p| p.is_finite() && (0.0..=1.0).contains(p)));
    let mut order: Vec<_> = (0..p.len()).collect();
    order.sort_by(|&a, &b| p[a].total_cmp(&p[b]));
    let mut q = vec![1.0; p.len()];
    let mut ceiling = 1.0_f64;
    for (rank, &index) in order.iter().enumerate().rev() {
        ceiling = ceiling.min(p[index] * p.len() as f64 / (rank + 1) as f64);
        q[index] = ceiling;
    }
    q
}

/// Exact two-sided Binomial(n, 1/2) sign test; callers drop zero returns.
/// Log probabilities avoid underflow from starting at 2^-n on long histories.
pub fn sign_test(positive: usize, negative: usize) -> Option<f64> {
    let n = positive.checked_add(negative)?;
    if n == 0 {
        return None;
    }
    let tail = positive.min(negative);
    let mut log_term = -(n as f64) * std::f64::consts::LN_2;
    let mut log_sum = log_term;
    for k in 1..=tail {
        log_term += ((n - k + 1) as f64 / k as f64).ln();
        let hi = log_sum.max(log_term);
        log_sum = hi + ((log_sum - hi).exp() + (log_term - hi).exp()).ln();
    }
    Some((2.0 * log_sum.exp()).min(1.0))
}

pub(crate) struct SplitMix64(pub u64);
impl SplitMix64 {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    pub(crate) fn below(&mut self, bound: usize) -> usize {
        let bound = bound as u64;
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let x = self.next();
            if x >= threshold {
                return (x % bound) as usize;
            }
        }
    }
}

/// Whole-block offsets, with a shared budget for all three default horizons.
pub(crate) fn rotation_offsets(
    n: usize,
    first_ts: i64,
    last_ts: i64,
    warm_len: usize,
    h_max: usize,
    horizon_count: usize,
) -> Vec<usize> {
    let l = 20.max(h_max.saturating_mul(2));
    if warm_len < l.saturating_mul(2) {
        return Vec::new();
    }
    let budget = (15_000_000 / n.max(1) / horizon_count.max(3)).min(999);
    let mut offsets: Vec<_> = (l..=warm_len - l).collect();
    if offsets.len() > budget {
        let mut seed = SplitMix64(n as u64);
        seed.0 ^= first_ts as u64;
        let mut rng = SplitMix64(seed.next() ^ last_ts as u64);
        // Partial Fisher–Yates: uniform sampling without replacement.
        for i in 0..budget {
            let j = i + rng.below(offsets.len() - i);
            offsets.swap(i, j);
        }
        offsets.truncate(budget);
    }
    offsets
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RotationResult {
    pub p: Option<f64>,
    pub band: Option<(f64, f64)>,
}

/// Evaluate all tests on the same circular offsets. The callback rotates the
/// signal block together, leaving the completed returns and their baseline fixed.
pub(crate) fn rotation_test(
    observed: &[Option<f64>],
    offsets: &[usize],
    mut statistic: impl FnMut(usize) -> Vec<Option<f64>>,
) -> Vec<RotationResult> {
    let mut samples = vec![Vec::with_capacity(offsets.len()); observed.len()];
    if offsets.len() >= 99 {
        for &offset in offsets {
            for (sample, d) in samples.iter_mut().zip(statistic(offset)) {
                if let Some(d) = d.filter(|d| d.is_finite()) {
                    sample.push(d);
                }
            }
        }
    }
    samples
        .iter_mut()
        .zip(observed)
        .map(|(sample, observed)| {
            let Some(d) = observed.filter(|d| d.is_finite()) else { return RotationResult::default() };
            if sample.len() < 99 {
                return RotationResult::default();
            }
            let extreme = sample.iter().filter(|x| x.abs() >= d.abs()).count();
            let p = (1 + extreme) as f64 / (sample.len() + 1) as f64;
            sample.sort_by(f64::total_cmp);
            let percentile = |p: f64| {
                let at = p * (sample.len() - 1) as f64;
                let lo = at.floor() as usize;
                let hi = at.ceil() as usize;
                sample[lo] + (sample[hi] - sample[lo]) * at.fract()
            };
            RotationResult { p: Some(p), band: Some((percentile(0.025), percentile(0.975))) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bh_hand_vector_ties_original_order_and_monotonicity() {
        let p = [0.04, 0.01, 0.03, 0.01, 0.5, 1.0];
        let q = benjamini_hochberg(&p);
        let expected = [0.06, 0.03, 0.06, 0.03, 0.6, 1.0];
        for (q, e) in q.iter().zip(expected) {
            assert!((q - e).abs() < 1e-12);
        }
        let mut order: Vec<_> = (0..p.len()).collect();
        order.sort_by(|&a, &b| p[a].total_cmp(&p[b]));
        assert!(order.windows(2).all(|w| q[w[0]] <= q[w[1]]));
        assert!(benjamini_hochberg(&[]).is_empty());
        assert_eq!(benjamini_hochberg(&[0.0, 1.0]), [0.0, 1.0]);
    }

    #[test]
    fn bh_is_step_up_not_step_down() {
        // m = 3, thresholds k·0.10/3 = .0333, .0667, .1. p(1) = .04 fails its own
        // threshold, but p(3) = .09 ≤ .1 so step-up rejects all three.
        // q(3) = .09; q(2) = min(.09, .05·3/2 = .075) = .075; q(1) = min(.075, .12) = .075.
        let q = benjamini_hochberg(&[0.09, 0.04, 0.05]);
        for (q, e) in q.iter().zip([0.09, 0.075, 0.075]) {
            assert!((q - e).abs() < 1e-12, "{q} vs {e}");
        }
        assert!(q.iter().all(|q| *q <= 0.10));
        // Adjusted values never exceed 1.
        assert_eq!(benjamini_hochberg(&[0.9, 0.95]), [0.95, 0.95]);
    }

    #[test]
    fn sign_test_hand_values_symmetry_and_long_balanced_sample() {
        assert_eq!(sign_test(0, 0), None);
        for (a, b, expected) in [(1, 0, 1.0), (5, 0, 0.0625), (4, 1, 0.375), (3, 3, 1.0), (9, 1, 0.021484375)] {
            assert!((sign_test(a, b).unwrap() - expected).abs() < 1e-12);
            assert_eq!(sign_test(a, b), sign_test(b, a));
        }
        assert!((sign_test(5000, 5000).unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn offsets_are_admissible_unique_deterministic_and_budgeted() {
        assert_eq!(rotation_offsets(200, 1, 200, 150, 20, 3), (40..=110).collect::<Vec<_>>());
        let a = rotation_offsets(5000, 1, 5000, 4900, 20, 3);
        assert_eq!(a, rotation_offsets(5000, 1, 5000, 4900, 20, 3));
        assert_ne!(a, rotation_offsets(5000, 2, 5000, 4900, 20, 3));
        assert_eq!(a.len(), 999);
        let mut unique = a.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(a.len(), unique.len());
        assert!(a.iter().all(|k| (40..=4860).contains(k)));
        let capped = rotation_offsets(100_000, 1, 100_000, 100_000, 20, 3);
        assert!(100_000 * capped.len() * 3 <= 15_000_000);
    }

    #[test]
    fn rotation_tail_band_and_minimum_offsets() {
        let offsets: Vec<_> = (1..=99).collect();
        let r = rotation_test(&[Some(50.0), Some(-50.0), None], &offsets, |k| {
            vec![Some(k as f64), Some(-(k as f64)), Some(k as f64)]
        });
        assert_eq!(r[0].p, Some(0.51));
        assert_eq!(r[0].p, r[1].p);
        assert_eq!(r[2], RotationResult::default());
        let (a, b) = r[0].band.unwrap();
        assert!((a - 3.45).abs() < 1e-12 && (b - 96.55).abs() < 1e-12);
        assert_eq!(
            rotation_test(&[Some(1.0)], &offsets[..98], |_| panic!("too few offsets"))[0],
            RotationResult::default()
        );
    }
}
