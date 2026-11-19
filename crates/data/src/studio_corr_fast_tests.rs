//! Bit-identity references for the lean rolling paths in `studio_corr`.
use super::*;

/// The pre-optimisation classic rolling loop, kept verbatim as the bit-identity reference.
fn reference_rolling(pairs: &[PairReturn], annual_periods: f64) -> Vec<Rolling> {
    ROLLING_WINDOWS
        .iter()
        .map(|&window| {
            let points = pairs
                .windows(window)
                .map(|w| {
                    let broken = w.windows(2).any(|p| p[0].end < p[1].start);
                    let m = regress(w, annual_periods).unwrap();
                    (w[window - 1].end, if broken { None } else { m.beta }, if broken { None } else { m.correlation })
                })
                .collect();
            Rolling { window, points }
        })
        .collect()
}

/// The pre-optimisation robust rolling loop (full `regress` + `fit_inference` per window).
fn reference_robust_rolling(pairs: &[PairReturn], annual_periods: f64, horizon: usize) -> Vec<Vec<BetaBandPoint>> {
    let breaks: Vec<_> = pairs.iter().enumerate().map(|(i, p)| i > 0 && pairs[i - 1].end < p.start).collect();
    let effective = |method| {
        if horizon > 1 && matches!(method, CovarianceMethod::HacFixed5 | CovarianceMethod::HacAutomatic) {
            CovarianceMethod::Overlap(horizon)
        } else {
            method
        }
    };
    let mut rolling = vec![Vec::new(); SE_METHODS.len()];
    for end in 60.max(pairs.len().saturating_sub(999))..=pairs.len() {
        let start = end - 60;
        let w = &pairs[start..end];
        let crosses = breaks[start + 1..end].iter().any(|b| *b);
        let m = regress(w, annual_periods).expect("admitted returns");
        for (i, &method) in SE_METHODS.iter().enumerate() {
            let fit = (!crosses).then(|| fit_inference(w, &m, &breaks[start..end], effective(method)));
            rolling[i].push(BetaBandPoint {
                ts: w[59].end,
                start: w[0].start,
                beta: (!crosses).then_some(m.beta).flatten(),
                covariance_label: fit
                    .as_ref()
                    .map(|f| f.covariance.label())
                    .unwrap_or_else(|| "window crosses a vault join".into()),
                interval: fit.and_then(|f| f.intervals[1]),
                break_before: breaks[end - 1] || crosses,
            });
        }
    }
    rolling
}

fn lcg(state: &mut u64) -> f64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    ((*state >> 11) as f64) / (1u64 << 53) as f64 - 0.5
}

/// 400 daily-spaced bars; the benchmark is flat for 70 bars (degenerate windows) and the
/// asset carries two vault gaps.
fn fixture(seed: u64) -> (NamedBars, NamedBars) {
    let mut s = seed;
    let (mut la, mut lb) = (4.0_f64, 4.0_f64);
    let (mut a, mut b) = (vec![], vec![]);
    for i in 0..400i64 {
        let x = if (200..270).contains(&i) { 0. } else { 0.01 * lcg(&mut s) };
        lb += x;
        la += 0.0003 + 1.2 * x + 0.008 * lcg(&mut s);
        let (pa, pb) = (la.exp(), lb.exp());
        a.push(Bar::new(i * 86_400, pa, pa, pa, pa, 1.).with_provenance(Provenance::Real));
        b.push(Bar::new(i * 86_400, pb, pb, pb, pb, 1.).with_provenance(Provenance::Real));
    }
    let gaps = vec![(a[90].ts, a[91].ts), (a[300].ts, a[301].ts)];
    (
        NamedBars { symbol: "A".into(), bars: a, gaps, ..Default::default() },
        NamedBars { symbol: "B".into(), bars: b, ..Default::default() },
    )
}

#[test]
fn corr_rolling_fast_paths_bit_identical() {
    for seed in [1, 2, 3] {
        let (a, b) = fixture(seed);
        let options = Options { annual_periods: 252., daily: false };
        for h in [1, 5, 20] {
            let r = compute_series_at_horizon(&a, &b, &[], options, "fixture", h).unwrap();
            assert!(r.returns.windows(2).any(|p| p[0].end < p[1].start), "fixture must contain breaks");
            let expected = reference_rolling(&r.returns, r.annual_periods);
            assert_eq!(r.rolling, expected, "classic rolling seed={seed} h={h}");
            assert!(r.rolling.iter().any(|x| x.points.iter().any(|p| p.1.is_none())));
            assert!(r.rolling.iter().any(|x| x.points.iter().any(|p| p.1.is_some())));
            let expected = reference_robust_rolling(&r.returns, r.annual_periods, h);
            assert_eq!(r.robust.rolling, expected, "robust rolling seed={seed} h={h}");
            assert!(r.robust.rolling.iter().all(|p| p.iter().any(|p| p.interval.is_some())));
            assert!(r.robust.rolling[2].iter().any(|p| p.interval.is_none()));
        }
    }
}

#[test]
fn lean_window_helpers_match_regress() {
    let (a, b) = fixture(9);
    let r =
        compute_series_at_horizon(&a, &b, &[], Options { annual_periods: 252., daily: false }, "fixture", 1).unwrap();
    for w in r.returns.windows(60).step_by(7) {
        let m = regress(w, 252.).unwrap();
        let core = core_fit(w);
        assert_eq!((core.alpha, core.beta, core.sigma), (m.alpha, m.beta, m.residual_sigma));
        assert_eq!(beta_corr(w), (m.beta, m.correlation));
    }
}
