//! Regime-conditioned predictive path quantiles, with causal walk-forward coverage.
//! Stationary bootstrap: Politis & Romano (1994). The resampling unit is an entire
//! completed forward log-return path, not a fitted mean or independent increments.
use crate::{closed::closed_len, nulls::SplitMix64, stats::require_research_bars};
use tr_core::{
    AssetClass, Bar, Timeframe,
    indicators::{AdaptiveTrendParams, trend_stack},
};

pub const QUANTILES: [f64; 5] = [0.10, 0.25, 0.50, 0.75, 0.90];
pub const MIN_PATHS: usize = 30;
const MAX_SCORES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub horizon: usize,
    pub mean_block: usize,
    pub draws: usize,
    pub seed: u64,
}
impl Default for Params {
    fn default() -> Self {
        Self { horizon: 20, mean_block: 5, draws: 2048, seed: 0x5452_454e_4443_4f4e }
    }
}
impl Params {
    pub fn validate(self) -> Result<(), String> {
        if !(1..=120).contains(&self.horizon)
            || !(1..=100).contains(&self.mean_block)
            || !(256..=8192).contains(&self.draws)
        {
            return Err("Trend Cone refused: horizon 1–120, mean block 1–100, draws 256–8192 required".into());
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
    if conditional.prices.iter().chain(&unconditional.prices).flatten().any(|v| !v.is_finite() || *v <= 0.0) {
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
        let seed = params.seed ^ bars[t].ts as u64;
        let conditional = horizon_quantiles(&conditional_train, params, seed);
        let unconditional = horizon_quantiles(&train, params, seed ^ 0x4241_5345);
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
fn bootstrap_indices(n: usize, params: Params, seed: u64) -> Vec<usize> {
    let mut rng = SplitMix64(seed);
    let mut indices = Vec::with_capacity(params.draws);
    while indices.len() < params.draws {
        let start = (rng.next() % n as u64) as usize;
        let length = block_length(&mut rng, params.mean_block).min(params.draws - indices.len());
        indices.extend((0..length).map(|k| (start + k) % n));
    }
    indices
}
fn quantiles(values: &mut [f64]) -> [f64; 5] {
    values.sort_by(f64::total_cmp);
    QUANTILES.map(|p| {
        let at = (values.len() - 1) as f64 * p;
        values[at.floor() as usize] * (1.0 - at.fract()) + values[at.ceil() as usize] * at.fract()
    })
}
fn horizon_quantiles(paths: &[&Path], params: Params, seed: u64) -> [f64; 5] {
    let mut samples: Vec<_> =
        bootstrap_indices(paths.len(), params, seed).iter().map(|&i| paths[i].log[params.horizon - 1]).collect();
    quantiles(&mut samples)
}
fn fan(paths: &[&Path], anchor: f64, params: Params, seed: u64) -> Fan {
    let indices = bootstrap_indices(paths.len(), params, seed);
    let mut prices = vec![[anchor; 5]];
    for k in 0..params.horizon {
        let mut values: Vec<_> = indices.iter().map(|&i| paths[i].log[k]).collect();
        prices.push(quantiles(&mut values).map(|r| (anchor.ln() + r).exp()));
    }
    Fan { prices, paths: paths.len() }
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
        Params { horizon: 5, draws: 512, ..Params::default() }
    }
    #[test]
    fn seeded_bootstrap_and_report_are_deterministic() {
        let b = bars(600);
        let s = vec![Some(1); b.len()];
        assert_eq!(build(&b, &s, params()).unwrap(), build(&b, &s, params()).unwrap());
        assert_ne!(bootstrap_indices(100, params(), 1), bootstrap_indices(100, params(), 2));
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
        let indices = bootstrap_indices(3, Params { mean_block: 100, ..params() }, 1);
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
