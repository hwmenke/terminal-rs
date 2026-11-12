//! Regime-conditioned predictive path quantiles, with causal walk-forward coverage.
//! The fan is the empirical per-step quantile of completed forward log-return
//! paths. The stationary bootstrap (Politis & Romano 1994) resamples whole path
//! rows in blocks of adjacent origins to measure how uncertain the horizon edges
//! are (`Fan::horizon_band`); it never widens or synthesizes the fan itself.
use crate::{closed::closed_len, nulls::SplitMix64, stats::require_research_bars};
use tr_core::{
    AssetClass, Bar, Timeframe,
    indicators::{AdaptiveTrendParams, trend_stack},
};

pub const QUANTILES: [f64; 5] = [0.10, 0.25, 0.50, 0.75, 0.90];
/// Nominal coverage of the central 25–75 and 10–90 bands.
pub const COVERAGE_NOMINAL: [f64; 2] = [0.50, 0.80];
pub const MIN_PATHS: usize = 30;
/// Walk-forward scores at most this many latest H-spaced (non-overlapping) origins.
pub const MAX_SCORES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub horizon: usize,
    pub mean_block: usize,
    pub draws: usize,
    pub seed: u64,
}
impl Default for Params {
    fn default() -> Self {
        // Overlapping H-step paths are dependent over ~H origins, so the default
        // mean block equals the default horizon; `CONE <H>` also keeps L = H.
        Self { horizon: 20, mean_block: 20, draws: 500, seed: 0x5452_454e_4443_4f4e }
    }
}
impl Params {
    pub fn validate(self) -> Result<(), String> {
        if !(1..=120).contains(&self.horizon)
            || !(1..=100).contains(&self.mean_block)
            || !(100..=4096).contains(&self.draws)
        {
            return Err(
                "Trend Cone refused: horizon 1–120, mean block 1–100, bootstrap replicates 100–4096 required".into()
            );
        }
        Ok(())
    }
}

/// Input fingerprint (including revisions to older OHLCV and provenance). The UI
/// memoizes construction by Arc identity; forming prices are deliberately not hashed.
#[derive(Debug, Clone, PartialEq)]
pub struct ConeKey {
    pub generation: u64,
    pub tf: Timeframe,
    pub closed: usize,
    pub params: Params,
    pub trend: AdaptiveTrendParams,
    fingerprint: u64,
}
impl ConeKey {
    pub fn new(
        generation: u64,
        tf: Timeframe,
        closed: usize,
        params: Params,
        trend: AdaptiveTrendParams,
        bars: &[Bar],
    ) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        bars.len().hash(&mut hash);
        for (i, b) in bars.iter().enumerate() {
            // A provenance downgrade anywhere, even on a forming bar, invalidates a report.
            (b.provenance as u8).hash(&mut hash);
            if i < closed {
                b.ts.hash(&mut hash);
                for v in [b.open, b.high, b.low, b.close, b.volume] {
                    v.to_bits().hash(&mut hash);
                }
            }
        }
        Self { generation, tf, closed, params, trend, fingerprint: hash.finish() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fan {
    /// Price quantiles at steps 0..=horizon. Step zero is the last CLOSED close.
    pub prices: Vec<[f64; 5]>,
    pub paths: usize,
    /// Stationary-bootstrap 90% interval (5th, 95th replicate percentile) of each
    /// horizon quantile, in price: sampling uncertainty of the edge, not a forecast.
    pub horizon_band: [[f64; 2]; 5],
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Coverage {
    pub n: usize,
    /// Inclusive central 50% (25–75) and 80% (10–90) band hits at the horizon.
    pub hits: [usize; 2],
    /// Observed CDF counts at each of the five quantile edges (ties count as <=).
    pub below: [usize; 5],
}
impl Coverage {
    fn observe(&mut self, bounds: [f64; 5], realized: f64) {
        self.n += 1;
        self.hits[0] += usize::from(bounds[1] <= realized && realized <= bounds[3]);
        self.hits[1] += usize::from(bounds[0] <= realized && realized <= bounds[4]);
        for (count, q) in self.below.iter_mut().zip(bounds) {
            *count += usize::from(realized <= q);
        }
    }
    pub fn empirical(&self, band: usize) -> Option<f64> {
        (self.n > 0).then(|| self.hits[band] as f64 / self.n as f64)
    }
    /// Calibration verdict for central band 0 (50%) or 1 (80%): `0` when the hit
    /// rate is within two binomial standard errors of nominal, `-1` under-covered
    /// (bands too narrow), `+1` over-covered (too wide). `None` below 10 outcomes.
    /// Scored outcomes are non-overlapping, so the binomial SE is a fair yardstick.
    pub fn verdict(&self, band: usize) -> Option<i8> {
        let nominal = COVERAGE_NOMINAL[band];
        let tolerance = 2.0 * (nominal * (1.0 - nominal) / self.n as f64).sqrt();
        let empirical = self.empirical(band).filter(|_| self.n >= 10)?;
        Some(if empirical < nominal - tolerance {
            -1
        } else if empirical > nominal + tolerance {
            1
        } else {
            0
        })
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct Forecast {
    pub origin: usize,
    pub training_last_end: usize,
    pub training_paths: usize,
    pub conditional: [f64; 5],
    pub unconditional: [f64; 5],
    pub realized: f64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ConeReport {
    pub closed: usize,
    pub anchor_ts: i64,
    pub anchor_price: f64,
    pub regime: i8,
    pub params: Params,
    pub conditional: Fan,
    pub unconditional: Fan,
    pub coverage: Coverage,
    pub baseline_coverage: Coverage,
    /// Non-overlapping evaluation outcomes, for the currently selected regime only.
    pub forecasts: Vec<Forecast>,
}
#[derive(Debug)]
struct Path {
    origin: usize,
    regime: i8,
    log: Vec<f64>,
}

/// Gate the complete input before slicing; compute TSTACK afresh on closed bars only.
/// No fetch, resampling of chart bars, interpolation or sample fallback is involved.
///
/// TSTACK's medium state is computed once over the closed prefix and reused for
/// every walk-forward origin. That is causal: every ingredient (adaptive MAs,
/// Wilder ATR, backward slopes, trailing efficiency ratio, sticky min-hold state,
/// per-session restarts) reads only bars `..=i` to produce state `i`, so it equals
/// the state a run truncated at `i` would see. `tstack_state_is_prefix_invariant`
/// pins this down.
pub fn report(
    bars: &[Bar],
    tf: Timeframe,
    class: AssetClass,
    as_of: Option<i64>,
    params: Params,
    trend: AdaptiveTrendParams,
) -> Result<ConeReport, String> {
    const OP: &str = "Trend Cone";
    Bar::require_verified(bars, OP)?;
    if bars.iter().any(|b| b.provenance != bars[0].provenance) {
        return Err(format!("{OP} refused: mixed real/cache history"));
    }
    let n = closed_len(tf, bars, class, as_of);
    let bars = &bars[..n];
    require_research_bars(bars, OP)?;
    params.validate()?;
    let stack = trend_stack(bars, tf, vec![], trend)?;
    let states: Vec<_> = stack.medium.iter().zip(&stack.confidence).map(|(&s, c)| c.is_finite().then_some(s)).collect();
    build(bars, &states, params)
}

fn build(bars: &[Bar], states: &[Option<i8>], params: Params) -> Result<ConeReport, String> {
    let n = bars.len();
    let regime =
        states.last().copied().flatten().ok_or("Trend Cone: TSTACK is warming up; load more verified history")?;
    let h = params.horizon;
    let paths: Vec<_> = (0..n.saturating_sub(h))
        .filter_map(|i| {
            Some(Path {
                origin: i,
                regime: states[i]?,
                log: (1..=h).map(|k| bars[i + k].close.ln() - bars[i].close.ln()).collect(),
            })
        })
        .collect();
    let all: Vec<_> = paths.iter().collect();
    let selected: Vec<_> = all.iter().copied().filter(|p| p.regime == regime).collect();
    if selected.len() < MIN_PATHS {
        return Err(format!(
            "Trend Cone: only {} completed paths in current regime; need {MIN_PATHS} · load more verified history or shorten CONE horizon",
            selected.len()
        ));
    }
    let anchor = bars.last().unwrap();
    let conditional = fan(&selected, anchor.close, params, params.seed);
    let unconditional = fan(&all, anchor.close, params, params.seed ^ 0x4241_5345);
    let bands = conditional.horizon_band.iter().chain(&unconditional.horizon_band).flatten();
    if conditional
        .prices
        .iter()
        .chain(&unconditional.prices)
        .flatten()
        .chain(bands)
        .any(|v| !v.is_finite() || *v <= 0.0)
    {
        return Err("Trend Cone refused: non-finite projected price".into());
    }
    let mut coverage = Coverage::default();
    let mut baseline_coverage = Coverage::default();
    let mut forecasts = Vec::new();
    // Fixed H-bar grid from history start: appending future bars cannot revise an
    // earlier forecast or change its seed. H-spaced scored outcomes do not overlap.
    let origins: Vec<_> =
        (0..n.saturating_sub(h)).step_by(h).filter(|&t| states[t] == Some(regime)).rev().take(MAX_SCORES).collect();
    for t in origins.into_iter().rev() {
        // At close t, only paths with endpoint <= t have matured. No t+1 input.
        let train: Vec<_> = paths.iter().take_while(|p| p.origin + h <= t).collect();
        let conditional_train: Vec<_> = train.iter().copied().filter(|p| p.regime == states[t].unwrap()).collect();
        if conditional_train.len() < MIN_PATHS {
            continue;
        }
        let conditional = horizon_quantiles(&conditional_train, h);
        let unconditional = horizon_quantiles(&train, h);
        let realized = bars[t + h].close.ln() - bars[t].close.ln();
        coverage.observe(conditional, realized);
        baseline_coverage.observe(unconditional, realized);
        forecasts.push(Forecast {
            origin: t,
            training_last_end: train.last().unwrap().origin + h,
            training_paths: conditional_train.len(),
            conditional,
            unconditional,
            realized,
        });
    }
    Ok(ConeReport {
        closed: n,
        anchor_ts: anchor.ts,
        anchor_price: anchor.close,
        regime,
        params,
        conditional,
        unconditional,
        coverage,
        baseline_coverage,
        forecasts,
    })
}

/// Geometric on {1,2,...}, success probability 1/L. Inverse-CDF draw avoids
/// per-increment restart loops; initial row uniform, continuation circular.
fn block_length(rng: &mut SplitMix64, mean: usize) -> usize {
    if mean == 1 {
        return 1;
    }
    let u = (rng.next() >> 11) as f64 * (1.0 / (1_u64 << 53) as f64);
    ((-u).ln_1p() / (-1.0 / mean as f64).ln_1p()).floor() as usize + 1
}
/// One stationary-bootstrap replicate: `len` row indices into a library of `n`
/// origin-ordered rows, in geometric blocks of adjacent rows with circular wrap.
fn bootstrap_indices(rng: &mut SplitMix64, n: usize, len: usize, mean_block: usize) -> Vec<usize> {
    let mut indices = Vec::with_capacity(len);
    while indices.len() < len {
        let start = rng.below(n);
        let length = block_length(rng, mean_block).min(len - indices.len());
        indices.extend((0..length).map(|k| (start + k) % n));
    }
    indices
}
/// Linear interpolation at `(len−1)p` of an ascending slice.
fn quantile_sorted(sorted: &[f64], p: f64) -> f64 {
    let at = (sorted.len() - 1) as f64 * p;
    sorted[at.floor() as usize] * (1.0 - at.fract()) + sorted[at.ceil() as usize] * at.fract()
}
fn quantiles(values: &mut [f64]) -> [f64; 5] {
    values.sort_by(f64::total_cmp);
    QUANTILES.map(|p| quantile_sorted(values, p))
}
/// Empirical horizon quantiles of a training library (log space). Deterministic.
fn horizon_quantiles(paths: &[&Path], h: usize) -> [f64; 5] {
    let mut samples: Vec<_> = paths.iter().map(|p| p.log[h - 1]).collect();
    quantiles(&mut samples)
}
/// The fan is the empirical per-step quantile of the completed-path library:
/// exact, with no Monte Carlo noise. (Resampling rows and re-taking quantiles
/// would only reproduce these values plus noise.) The stationary bootstrap answers
/// what the plain quantiles cannot: how stable are the horizon edges, given that
/// overlapping paths are strongly dependent? Each replicate redraws a library of
/// the same size in blocks of adjacent origins; the 5th–95th percentile of each
/// replicate's quantile is reported as `horizon_band`.
fn fan(paths: &[&Path], anchor: f64, params: Params, seed: u64) -> Fan {
    let h = params.horizon;
    let mut prices = vec![[anchor; 5]];
    for k in 0..h {
        let mut values: Vec<_> = paths.iter().map(|p| p.log[k]).collect();
        prices.push(quantiles(&mut values).map(|r| anchor * r.exp()));
    }
    let mut rng = SplitMix64(seed);
    let mut replicates: [Vec<f64>; 5] = Default::default();
    let mut values = Vec::with_capacity(paths.len());
    for _ in 0..params.draws {
        values.clear();
        let rows = bootstrap_indices(&mut rng, paths.len(), paths.len(), params.mean_block);
        values.extend(rows.iter().map(|&i| paths[i].log[h - 1]));
        for (out, q) in replicates.iter_mut().zip(quantiles(&mut values)) {
            out.push(q);
        }
    }
    let horizon_band = replicates.map(|mut r| {
        r.sort_by(f64::total_cmp);
        [anchor * quantile_sorted(&r, 0.05).exp(), anchor * quantile_sorted(&r, 0.95).exp()]
    });
    Fan { prices, paths: paths.len(), horizon_band }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::Provenance;
    fn bars(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let p = 100.0 * (0.0002 * i as f64 + 0.025 * (i as f64 * 0.13).sin()).exp();
                Bar::new(i as i64 * 86400, p, p + 0.1, p - 0.1, p, 1.0).with_provenance(Provenance::Real)
            })
            .collect()
    }
    fn params() -> Params {
        Params { horizon: 5, mean_block: 5, draws: 200, ..Params::default() }
    }
    #[test]
    fn seeded_bootstrap_and_report_are_deterministic() {
        let b = bars(600);
        let s = vec![Some(1); b.len()];
        assert_eq!(build(&b, &s, params()).unwrap(), build(&b, &s, params()).unwrap());
        let draw = |seed| bootstrap_indices(&mut SplitMix64(seed), 100, 100, 5);
        assert_eq!(draw(1), draw(1));
        assert_ne!(draw(1), draw(2));
        let other_seed = build(&b, &s, Params { seed: 7, ..params() }).unwrap();
        let base = build(&b, &s, params()).unwrap();
        // The seed only moves the bootstrap uncertainty band, never the fan or scores.
        assert_eq!(other_seed.conditional.prices, base.conditional.prices);
        assert_eq!(other_seed.forecasts, base.forecasts);
        assert_ne!(other_seed.conditional.horizon_band, base.conditional.horizon_band);
    }
    #[test]
    fn fan_is_the_exact_empirical_path_quantile() {
        let b = bars(400);
        let r = build(&b, &vec![Some(1); b.len()], params()).unwrap();
        let anchor = b.last().unwrap().close;
        for k in 1..=5 {
            let mut logs: Vec<_> = (0..b.len() - 5).map(|i| (b[i + k].close / b[i].close).ln()).collect();
            logs.sort_by(f64::total_cmp);
            for (q, p) in QUANTILES.iter().enumerate() {
                let expected = anchor * quantile_sorted(&logs, *p).exp();
                assert!((r.conditional.prices[k][q] - expected).abs() < 1e-9 * expected, "k={k} q={q}");
            }
        }
    }
    #[test]
    fn bootstrap_band_brackets_each_horizon_edge_and_widens_with_dependence() {
        let b = bars(600);
        let s = vec![Some(1); b.len()];
        let r = build(&b, &s, params()).unwrap();
        let end = r.conditional.prices.last().unwrap();
        for (q, [lo, hi]) in r.conditional.horizon_band.iter().enumerate() {
            assert!(lo <= hi && *lo <= end[q] * 1.000_001 && end[q] <= hi * 1.000_001, "q={q}");
        }
        assert!(r.conditional.horizon_band.iter().any(|[lo, hi]| hi > lo));
        // Path rows from adjacent origins overlap; longer blocks keep that
        // dependence and must report more edge uncertainty than i.i.d. rows.
        let width = |r: &ConeReport| r.conditional.horizon_band.iter().map(|[lo, hi]| hi - lo).sum::<f64>();
        let iid = build(&b, &s, Params { mean_block: 1, ..params() }).unwrap();
        let blocky = build(&b, &s, Params { mean_block: 60, ..params() }).unwrap();
        assert!(width(&blocky) > 1.1 * width(&iid), "{} vs {}", width(&blocky), width(&iid));
    }
    #[test]
    fn tstack_state_is_prefix_invariant() {
        // The cone reuses one TSTACK pass for every walk-forward origin. That is
        // only legitimate if state i equals the state of a run truncated at i.
        for (tf, step, session_reset) in
            [(Timeframe::D1, 86_400, false), (Timeframe::M5, 300, false), (Timeframe::M5, 300, true)]
        {
            let b: Vec<_> = bars(420)
                .into_iter()
                .enumerate()
                .map(|(i, mut bar)| {
                    bar.ts = 1_700_000_000 + i as i64 * step;
                    bar
                })
                .collect();
            let trend = AdaptiveTrendParams { session_reset, ..AdaptiveTrendParams::default() };
            let full = trend_stack(&b, tf, vec![], trend).unwrap();
            for t in (60..b.len()).step_by(17) {
                let prefix = trend_stack(&b[..=t], tf, vec![], trend).unwrap();
                assert_eq!(prefix.medium[t], full.medium[t], "{tf:?} reset={session_reset} t={t}");
                assert_eq!(
                    prefix.confidence[t].to_bits(),
                    full.confidence[t].to_bits(),
                    "{tf:?} reset={session_reset} t={t}"
                );
            }
        }
    }
    #[test]
    fn coverage_verdict_uses_binomial_tolerance() {
        let cov = |n, hits| Coverage { n, hits: [hits, hits], below: [0; 5] };
        assert_eq!(cov(9, 9).verdict(0), None);
        assert_eq!(cov(100, 50).verdict(0), Some(0));
        assert_eq!(cov(100, 39).verdict(0), Some(-1)); // 2 SE = 10 points
        assert_eq!(cov(100, 61).verdict(0), Some(1));
        assert_eq!(cov(100, 73).verdict(1), Some(0)); // 2 SE = 8 points
        assert_eq!(cov(100, 71).verdict(1), Some(-1));
    }
    #[test]
    fn coverage_math_has_a_known_answer_including_ties() {
        let mut c = Coverage::default();
        for value in [-2.0, -0.5, 0.0, 0.5, 2.0] {
            c.observe([-1.0, -0.5, 0.0, 0.5, 1.0], value);
        }
        assert_eq!(c.n, 5);
        assert_eq!(c.hits, [3, 3]);
        assert_eq!(c.below, [1, 2, 3, 4, 4]);
        assert_eq!(c.empirical(0), Some(0.6));
        assert_eq!(Coverage::default().empirical(1), None);
    }
    #[test]
    fn flat_synthetic_paths_have_exact_walk_forward_coverage() {
        let mut b = bars(300);
        for bar in &mut b {
            bar.open = 100.0;
            bar.close = 100.0;
            bar.high = 100.0;
            bar.low = 100.0;
        }
        let r = build(&b, &vec![Some(0); b.len()], params()).unwrap();
        assert_eq!(r.coverage.n, 52); // completed origins 35,40,...,290
        assert_eq!(r.coverage.hits, [52, 52]);
        assert_eq!(r.coverage.below, [52; 5]);
        assert!(r.forecasts.iter().all(|f| f.conditional == [0.0; 5]));
        assert_eq!(r.conditional.horizon_band, [[100.0; 2]; 5]);
    }
    #[test]
    fn forming_bar_never_changes_anchor_samples_regime_or_score() {
        for tf in [Timeframe::M5, Timeframe::D1] {
            let mut b = bars(500);
            let r = report(&b, tf, AssetClass::Crypto, None, params(), AdaptiveTrendParams::default()).unwrap();
            assert_eq!(r.closed, 499);
            assert_eq!(r.anchor_ts, b[498].ts);
            b[499].close *= 100.0;
            b[499].high = b[499].close;
            assert_eq!(r, report(&b, tf, AssetClass::Crypto, None, params(), AdaptiveTrendParams::default()).unwrap());
            let key = ConeKey::new(1, tf, 499, params(), AdaptiveTrendParams::default(), &b);
            b[499].close *= 2.0;
            assert_eq!(key, ConeKey::new(1, tf, 499, params(), AdaptiveTrendParams::default(), &b));
        }
    }
    #[test]
    fn refuses_sample_unverified_mixed_and_insufficient_history() {
        for provenance in [Provenance::Sample, Provenance::Missing, Provenance::Cached] {
            let mut b = bars(200);
            b[199].provenance = provenance;
            assert!(
                report(&b, Timeframe::D1, AssetClass::Equity, None, params(), AdaptiveTrendParams::default()).is_err()
            );
        }
        assert!(
            report(&[], Timeframe::D1, AssetClass::Equity, None, params(), AdaptiveTrendParams::default()).is_err()
        );
        assert!(build(&bars(20), &[Some(1); 20], params()).unwrap_err().contains("need 30"));
    }
    #[test]
    fn quantiles_are_monotone_at_every_step_and_anchor_at_closed_close() {
        let b = bars(500);
        let r = build(&b, &vec![Some(1); b.len()], params()).unwrap();
        for fan in [&r.conditional, &r.unconditional] {
            assert_eq!(fan.prices[0], [b.last().unwrap().close; 5]);
            assert_eq!(fan.prices.len(), 6);
            assert!(fan.prices.iter().all(|q| q.windows(2).all(|w| w[0] <= w[1])));
        }
    }
    #[test]
    fn regime_conditioning_selects_origin_state_and_only_completed_paths() {
        let mut b = bars(400);
        for (i, bar) in b.iter_mut().enumerate() {
            bar.close = if i < 200 { (0.01 * i as f64).exp() } else { (2.0 - 0.01 * (i - 200) as f64).exp() };
        }
        let s: Vec<_> = (0..400).map(|i| Some(if i < 200 { 1 } else { -1 })).collect();
        let r = build(&b, &s, params()).unwrap();
        assert_eq!(r.regime, -1);
        assert_eq!(r.conditional.paths, 195);
        assert_eq!(r.unconditional.paths, 395);
        assert!(r.conditional.prices[5][4] < r.anchor_price);
        assert!(r.unconditional.prices[5][4] > r.anchor_price);
    }
    #[test]
    fn walk_forward_is_purged_and_future_append_cannot_revise_a_forecast() {
        let b = bars(600);
        let s = vec![Some(1); 600];
        let a = build(&b[..400], &s[..400], params()).unwrap();
        let z = build(&b, &s, params()).unwrap();
        for f in &a.forecasts {
            assert!(f.training_last_end <= f.origin);
            if let Some(later) = z.forecasts.iter().find(|g| g.origin == f.origin) {
                assert_eq!(f, later);
            }
        }
        assert!(a.forecasts.windows(2).all(|w| w[1].origin >= w[0].origin + 5));
        // Independently reconstruct the training set at an evaluation close.
        let f = a.forecasts.last().unwrap();
        let prefix = build(&b[..=f.origin], &s[..=f.origin], params()).unwrap();
        assert_eq!(f.training_paths, prefix.conditional.paths);
        // A cone built on history truncated at the origin reproduces the forecast.
        for q in 0..5 {
            let truncated = prefix.conditional.prices[5][q];
            let scored = b[f.origin].close * f.conditional[q].exp();
            assert!((truncated - scored).abs() < 1e-9 * scored, "q={q}");
        }
    }
    #[test]
    fn geometric_block_lengths_match_mean_and_survival_and_wrap_circularly() {
        let mut rng = SplitMix64(47);
        let lengths: Vec<_> = (0..100_000).map(|_| block_length(&mut rng, 5)).collect();
        let mean = lengths.iter().sum::<usize>() as f64 / lengths.len() as f64;
        let survival = lengths.iter().filter(|&&n| n > 5).count() as f64 / lengths.len() as f64;
        assert!((mean - 5.0).abs() < 0.07, "{mean}");
        assert!((survival - 0.8_f64.powi(5)).abs() < 0.008, "{survival}");
        assert_eq!(block_length(&mut rng, 1), 1);
        let indices = bootstrap_indices(&mut SplitMix64(1), 3, 512, 100);
        assert_eq!(indices.len(), 512);
        assert!(indices.windows(2).filter(|w| w[1] == (w[0] + 1) % 3).count() > 450);
        assert!(indices.windows(2).any(|w| w == [2, 0]));
    }
    #[test]
    fn cache_key_catches_older_price_and_provenance_revisions() {
        let mut b = bars(100);
        let key = ConeKey::new(1, Timeframe::D1, 99, params(), AdaptiveTrendParams::default(), &b);
        b[10].high += 1.0;
        assert_ne!(key, ConeKey::new(1, Timeframe::D1, 99, params(), AdaptiveTrendParams::default(), &b));
        b[10].high -= 1.0;
        b[99].provenance = Provenance::Sample;
        assert_ne!(key, ConeKey::new(1, Timeframe::D1, 99, params(), AdaptiveTrendParams::default(), &b));
    }
}
