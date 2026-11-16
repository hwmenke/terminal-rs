//! Causal closed-bar ATR guides and same-distance empirical stop-touch odds.
//! No fetching, interpolation, synthetic prices or trading side effects.
use crate::{closed::closed_len, edge::wilson95, stats::require_research_bars};
use tr_core::{
    AssetClass, Bar, Timeframe,
    indicators::{AdaptiveTrendParams, base_trend, stop_guide},
};

pub const N_MIN: usize = 20;
pub const E_MIN: usize = 5;
#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub ks: Vec<f64>,
    pub horizons: Option<(usize, usize)>,
}
impl Default for Params {
    fn default() -> Self {
        Self { ks: vec![1.0, 2.0, 3.0], horizons: None }
    }
}
impl Params {
    pub fn validate(&self) -> Result<(), String> {
        if self.ks.is_empty()
            || self.ks.len() > 3
            || self.ks.iter().any(|k| !k.is_finite() || !(0.25..=6.0).contains(k))
            || self.ks.windows(2).any(|w| w[0] >= w[1])
        {
            return Err("STOPS K needs one to three ascending ATR multiples in 0.25–6".into());
        }
        if self.horizons.is_some_and(|(a, b)| a == 0 || a >= b || b > 250) {
            return Err("STOPS H needs two ascending horizons in 1–250 bars".into());
        }
        Ok(())
    }
}
pub fn default_horizons(tf: Timeframe) -> (usize, usize) {
    match tf {
        Timeframe::M1 => (15, 60),
        Timeframe::M5 => (12, 48),
        Timeframe::M15 => (8, 26),
        Timeframe::M30 => (4, 13),
        Timeframe::H1 => (7, 35),
        Timeframe::H4 => (2, 10),
        Timeframe::D1 => (5, 20),
        Timeframe::W1 => (4, 13),
        Timeframe::MN1 => (3, 12),
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct StopKey {
    pub generation: u64,
    pub tf: Timeframe,
    pub closed: usize,
    pub params: Params,
    pub trend: AdaptiveTrendParams,
    fingerprint: u64,
}
impl StopKey {
    pub fn new(
        generation: u64,
        tf: Timeframe,
        closed: usize,
        params: Params,
        trend: AdaptiveTrendParams,
        bars: &[Bar],
    ) -> Self {
        let mut hash = crate::stable_hash::Fnv64::default();
        hash.write_u64(bars.len() as u64);
        hash.write_u64(crate::stable_hash::bars_digest(&bars[..closed.min(bars.len())]));
        // Include full provenance even on the forming bar: changing Real to Cached
        // can introduce an inadmissible mixed series without changing any price.
        for b in bars {
            hash.write_u64(b.provenance as u64);
        }
        Self { generation, tf, closed, params, trend, fingerprint: hash.finish() }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum Touch {
    Ok { hits: usize, n: usize, p: f64, lo: f64, hi: f64 },
    Through,
    LowN { n: usize, episodes: usize },
    NoGuide,
}
impl Touch {
    pub fn reason(&self) -> String {
        match self {
            Self::LowN { n, .. } if *n < N_MIN => format!("n={n} < {N_MIN}"),
            Self::LowN { episodes, .. } => format!("episodes={episodes} < {E_MIN}"),
            Self::NoGuide => "no valid stop side / ATR is zero".into(),
            Self::Through => "last closed close is already through the guide".into(),
            Self::Ok { .. } => String::new(),
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub origin: usize,
    pub episode: usize,
    pub mae: f64,
    pub mfe: f64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct HorizonOdds {
    pub h: usize,
    pub n: usize,
    pub episodes: usize,
    pub span: Option<(i64, i64)>,
    pub skipped_holes: usize,
    pub skipped_bad: usize,
    pub crossing_sessions: usize,
    pub mae: Option<(f64, f64)>,
    pub mfe: Option<(f64, f64)>,
    pub touches: [Touch; 3],
    pub from_close: [Touch; 3],
    pub all_origins: [Touch; 3],
    /// Auditable non-overlapping observations. Each future window is origin+1..=origin+H.
    pub outcomes: Vec<Outcome>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct StopReport {
    pub closed: usize,
    pub anchor_ts: i64,
    pub regime: i8,
    pub params: Params,
    pub mb: Vec<f64>,
    pub atr: Vec<f64>,
    pub state: Vec<i8>,
    pub guides: [Vec<f64>; 3],
    pub close: f64,
    pub current_atr: f64,
    pub levels: [f64; 3],
    pub distances: [f64; 3],
    pub horizons: [HorizonOdds; 2],
}
pub fn report(
    bars: &[Bar],
    tf: Timeframe,
    class: AssetClass,
    as_of: Option<i64>,
    params: Params,
    trend: AdaptiveTrendParams,
) -> Result<StopReport, String> {
    const OP: &str = "Stop Cloud";
    Bar::require_verified(bars, OP)?;
    if bars.iter().any(|b| b.provenance != bars[0].provenance) {
        return Err(format!("{OP} refused: mixed real/cache history"));
    }
    let n = closed_len(tf, bars, class, as_of);
    let bars = &bars[..n];
    require_research_bars(bars, OP)?;
    params.validate()?;
    let t = base_trend(bars, tf, trend)?;
    if !t.confidence.last().is_some_and(|c| c.is_finite()) {
        return Err("Stop Cloud: Adaptive Trend is warming up; load more verified history".into());
    }
    let state =
        t.medium_state.iter().zip(&t.confidence).map(|(&s, c)| if c.is_finite() { s } else { 0 }).collect::<Vec<_>>();
    let mb = t.mb.iter().zip(&t.confidence).map(|(&mb, c)| if c.is_finite() { mb } else { f64::NAN }).collect();
    Ok(build(bars, tf, class, params, mb, t.atr, state))
}
fn usable(b: &Bar) -> bool {
    [b.open, b.high, b.low, b.close].iter().all(|v| v.is_finite()) && b.high >= b.low
}
fn quantiles(mut values: Vec<f64>) -> Option<(f64, f64)> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let q = |p: f64| {
        let x = p * (values.len() - 1) as f64;
        let i = x.floor() as usize;
        values[i] + (values[x.ceil() as usize] - values[i]) * x.fract()
    };
    Some((q(0.5), q(0.8)))
}
fn touch(d: f64, outcomes: &[Outcome], episodes: usize) -> Touch {
    if !d.is_finite() {
        return Touch::NoGuide;
    }
    if d <= 0.0 {
        return Touch::Through;
    }
    let n = outcomes.len();
    if n < N_MIN || episodes < E_MIN {
        return Touch::LowN { n, episodes };
    }
    let hits = outcomes.iter().filter(|o| o.mae >= d).count();
    let p = hits as f64 / n as f64;
    let (lo, hi) = wilson95(hits, n).expect("nonempty trial count");
    Touch::Ok { hits, n, p, lo, hi }
}
fn episode_count(outcomes: &[Outcome]) -> usize {
    let mut ids: Vec<_> = outcomes.iter().map(|o| o.episode).collect();
    ids.dedup();
    ids.len()
}
#[allow(clippy::too_many_arguments)]
fn horizon(
    bars: &[Bar],
    tf: Timeframe,
    class: AssetClass,
    state: &[i8],
    atr: &[f64],
    regime: i8,
    h: usize,
    distances: [f64; 3],
    ks: &[f64],
) -> HorizonOdds {
    let mut r = HorizonOdds {
        h,
        n: 0,
        episodes: 0,
        span: None,
        skipped_holes: 0,
        skipped_bad: 0,
        crossing_sessions: 0,
        mae: None,
        mfe: None,
        touches: std::array::from_fn(|_| Touch::NoGuide),
        from_close: std::array::from_fn(|_| Touch::NoGuide),
        all_origins: std::array::from_fn(|_| Touch::NoGuide),
        outcomes: vec![],
    };
    if regime == 0 {
        return r;
    }
    let equity = matches!(class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index);
    let mut next_origin = 0;
    let mut episode = 0;
    let mut all = vec![];
    for t in 0..bars.len().saturating_sub(h) {
        if t > 0 && state[t] != state[t - 1] {
            episode += 1;
        }
        if state[t] != regime || !atr[t].is_finite() || atr[t] <= 0.0 {
            continue;
        }
        let accepted = t >= next_origin;
        // Reserve before validation: dropped windows are counted, never replaced.
        if accepted {
            next_origin = t + h;
        }
        let window = &bars[t..=t + h];
        if window.iter().any(|b| !usable(b)) {
            if accepted {
                r.skipped_bad += 1;
            }
            continue;
        }
        let crosses = window.windows(2).any(|w| tr_core::ny::date_key(w[0].ts) != tr_core::ny::date_key(w[1].ts));
        let hole = tf.is_intraday()
            && window.windows(2).any(|w| {
                w[1].ts - w[0].ts > tf.seconds()
                    && (!equity || tr_core::ny::date_key(w[0].ts) == tr_core::ny::date_key(w[1].ts))
            });
        if hole {
            if accepted {
                r.skipped_holes += 1;
            }
            continue;
        }
        let lo = bars[t + 1..=t + h].iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
        let hi = bars[t + 1..=t + h].iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max);
        let c = bars[t].close;
        let (mae, mfe) =
            if regime == 1 { ((c - lo) / atr[t], (hi - c) / atr[t]) } else { ((hi - c) / atr[t], (c - lo) / atr[t]) };
        let outcome = Outcome { origin: t, episode, mae: mae.max(0.0), mfe: mfe.max(0.0) };
        all.push(outcome.clone());
        if accepted {
            r.crossing_sessions += usize::from(crosses);
            r.span = Some((r.span.map_or(bars[t].ts, |s| s.0), bars[t + h].ts));
            r.outcomes.push(outcome);
        }
    }
    r.n = r.outcomes.len();
    r.episodes = episode_count(&r.outcomes);
    r.mae = quantiles(r.outcomes.iter().map(|o| o.mae).collect());
    r.mfe = quantiles(r.outcomes.iter().map(|o| o.mfe).collect());
    for (k, &multiple) in ks.iter().enumerate() {
        r.touches[k] = touch(distances[k], &r.outcomes, r.episodes);
        r.from_close[k] = touch(multiple, &r.outcomes, r.episodes);
        r.all_origins[k] = touch(distances[k], &all, episode_count(&all));
    }
    r
}
fn build(
    bars: &[Bar],
    tf: Timeframe,
    class: AssetClass,
    params: Params,
    mb: Vec<f64>,
    atr: Vec<f64>,
    state: Vec<i8>,
) -> StopReport {
    let n = bars.len();
    let regime = state[n - 1];
    let guides: [Vec<f64>; 3] = std::array::from_fn(|k| {
        let mut g = params.ks.get(k).map_or_else(|| vec![f64::NAN; n], |&k| stop_guide(&mb, &state, &atr, k));
        for i in 0..n {
            if state[i] == 0 || !mb[i].is_finite() || !atr[i].is_finite() {
                g[i] = f64::NAN;
            }
        }
        g
    });
    let close = bars[n - 1].close;
    let current_atr = atr[n - 1];
    let levels = std::array::from_fn(|k| guides[k][n - 1]);
    let distances = levels.map(|level| {
        if regime == 0 || current_atr <= 0.0 { f64::NAN } else { regime as f64 * (close - level) / current_atr }
    });
    let (a, b) = params.horizons.unwrap_or_else(|| default_horizons(tf));
    let horizons = [a, b].map(|h| horizon(bars, tf, class, &state, &atr, regime, h, distances, &params.ks));
    StopReport {
        closed: n,
        anchor_ts: bars[n - 1].ts,
        regime,
        params,
        mb,
        atr,
        state,
        guides,
        close,
        current_atr,
        levels,
        distances,
        horizons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::Provenance;
    fn bars(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                Bar::new(1_700_000_000 + i as i64 * 300, 100.0, 102.0, 98.0, 100.0, 1.0)
                    .with_provenance(Provenance::Real)
            })
            .collect()
    }
    fn states(n: usize) -> Vec<i8> {
        (0..n).map(|i| if i / 12 % 2 == 0 { 1 } else { -1 }).collect()
    }
    fn odds(b: &[Bar], state: &[i8], atr: &[f64], sign: i8, h: usize, ds: [f64; 3]) -> HorizonOdds {
        horizon(b, Timeframe::M5, AssetClass::Crypto, state, atr, sign, h, ds, &[1.0, 2.0, 3.0])
    }
    fn built(b: &[Bar], state: Vec<i8>) -> StopReport {
        build(b, Timeframe::M5, AssetClass::Crypto, Params::default(), vec![100.0; b.len()], vec![2.0; b.len()], state)
    }
    fn bits(v: &[f64]) -> Vec<u64> {
        v.iter().map(|v| v.to_bits()).collect()
    }
    #[test]
    fn envelope_values_on_constant_range_bars() {
        let mut b = bars(180);
        for b in &mut b {
            b.high = 101.0;
            b.low = 99.0;
        }
        let r = report(&b, Timeframe::D1, AssetClass::Crypto, None, Params::default(), Default::default()).unwrap();
        assert_eq!(r.regime, 0);
        for i in 60..r.closed {
            assert_eq!(r.atr[i], 2.0);
            assert!((r.mb[i] - 100.0).abs() < 1e-10);
            for k in &r.params.ks {
                assert!((r.mb[i] + k * r.atr[i] - (100.0 + 2.0 * k)).abs() < 1e-10);
            }
        }
    }
    #[test]
    fn touch_counts_on_known_paths() {
        let mut b = bars(241);
        for (i, b) in b.iter_mut().enumerate() {
            b.low = 100.0 - (i % 4) as f64;
        }
        let s = states(b.len());
        let r = odds(&b, &s, &vec![1.0; b.len()], 1, 1, [1.0, 2.0, 3.0]);
        assert_eq!(r.n, 120);
        assert_eq!(r.episodes, 10);
        for (k, hits) in [90, 60, 30].into_iter().enumerate() {
            assert!(matches!(r.touches[k],Touch::Ok{hits:n,n:120,..} if n==hits));
        }
        assert!(b.iter().all(|b| b.close == 100.0)); // Close-only counting would miss every hit.
    }
    #[test]
    fn short_regime_mirrors_long() {
        let b = bars(241);
        let s = states(b.len());
        let a = vec![2.0; b.len()];
        let long = odds(&b, &s, &a, 1, 2, [0.5, 1.0, 1.5]);
        let mirror: Vec<_> = b.iter().map(|b| Bar { high: 200.0 - b.low, low: 200.0 - b.high, ..*b }).collect();
        let short = odds(&mirror, &s.iter().map(|s| -s).collect::<Vec<_>>(), &a, -1, 2, [0.5, 1.0, 1.5]);
        assert_eq!(long, short);
    }
    #[test]
    fn gap_through_the_level_counts_as_a_touch() {
        let mut b = bars(241);
        let s = states(b.len());
        for b in &mut b[1..] {
            b.open = 95.0;
            b.low = 94.0;
            b.high = 96.0;
            b.close = 95.0;
        }
        b[0].close = 100.0;
        let r = odds(&b, &s, &vec![2.0; b.len()], 1, 1, [2.0, 3.0, 4.0]);
        assert_eq!(r.outcomes[0].mae, 3.0);
        assert!(r.outcomes[0].mae >= 3.0); // stop 94 exactly, despite opening below 96.
    }
    #[test]
    fn mae_mfe_units_and_quantiles_match_hand_values() {
        let mut b = bars(5);
        b[1].low = 96.0;
        b[1].high = 106.0;
        b[2].low = 98.0;
        b[2].high = 108.0;
        let r = odds(&b, &[1; 5], &[2.0; 5], 1, 2, [1.0; 3]);
        assert_eq!(r.outcomes[0].mae, 2.0);
        assert_eq!(r.outcomes[0].mfe, 4.0);
        assert_eq!(quantiles(vec![0.0, 1.0, 2.0, 3.0, 4.0]), Some((2.0, 3.2)));
        assert_eq!(quantiles(vec![]), None);
    }
    #[test]
    fn origins_are_h_spaced_disjoint_and_prefix_stable() {
        let b = bars(501);
        let s = states(b.len());
        let a = vec![2.0; b.len()];
        for h in [1, 5, 12, 48] {
            let short = odds(&b[..300], &s[..300], &a[..300], 1, h, [1.0; 3]);
            let full = odds(&b, &s, &a, 1, h, [1.0; 3]);
            assert!(full.outcomes.windows(2).all(|w| w[1].origin >= w[0].origin + h));
            assert_eq!(&full.outcomes[..short.n], short.outcomes);
            assert!(full.outcomes.iter().all(|o| o.origin + h < b.len()));
        }
    }
    #[test]
    fn only_same_sign_valid_origins_enter() {
        let b = bars(301);
        let mut s = states(b.len());
        let mut a = vec![2.0; b.len()];
        s[0] = 0;
        a[2] = 0.0;
        a[3] = f64::NAN;
        let r = odds(&b, &s, &a, 1, 1, [1.0; 3]);
        assert!(r.outcomes.iter().all(|o| s[o.origin] == 1 && a[o.origin] > 0.0));
        assert!(!r.outcomes.iter().any(|o| [0, 2, 3].contains(&o.origin)));
    }
    #[test]
    fn no_look_ahead_mutating_bars_after_t_plus_h_leaves_outcome_t_unchanged() {
        let mut b = bars(301);
        let s = states(b.len());
        let a = vec![2.0; b.len()];
        let before = odds(&b, &s, &a, 1, 5, [1.0; 3]);
        for b in &mut b[6..] {
            b.high *= 10.0;
            b.low /= 10.0;
        }
        assert_eq!(before.outcomes[0], odds(&b, &s, &a, 1, 5, [1.0; 3]).outcomes[0]);
    }
    #[test]
    fn series_are_prefix_invariant() {
        for (tf, reset) in [(Timeframe::D1, false), (Timeframe::M5, false), (Timeframe::M5, true)] {
            let mut b = bars(500);
            for (i, b) in b.iter_mut().enumerate() {
                let c = 100.0 + 5.0 * (i as f64 * 0.07).sin();
                b.close = c;
                b.open = c;
                b.low = c - 1.0;
                b.high = c + 1.0;
            }
            let trend = AdaptiveTrendParams { session_reset: reset, ..Default::default() };
            let full = base_trend(&b, tf, trend).unwrap();
            let guide = stop_guide(&full.mb, &full.medium_state, &full.atr, 2.0);
            for n in (100..b.len()).step_by(29) {
                let prefix = base_trend(&b[..n], tf, trend).unwrap();
                assert_eq!(bits(&full.atr[..n]), bits(&prefix.atr));
                assert_eq!(bits(&full.mb[..n]), bits(&prefix.mb));
                assert_eq!(&full.medium_state[..n], prefix.medium_state);
                assert_eq!(bits(&guide[..n]), bits(&stop_guide(&prefix.mb, &prefix.medium_state, &prefix.atr, 2.0)));
            }
        }
    }
    #[test]
    fn forming_bar_is_excluded_and_mutating_it_changes_nothing() {
        let mut b = bars(301);
        let run = |b: &[Bar]| {
            report(b, Timeframe::M5, AssetClass::Crypto, None, Params::default(), Default::default()).unwrap()
        };
        let before = run(&b);
        b[300].close = 1e6;
        b[300].low = f64::NAN;
        let after = run(&b);
        assert_eq!(before.closed, 300);
        assert_eq!(before.horizons, after.horizons);
        assert_eq!(bits(&before.mb), bits(&after.mb));
        assert_eq!(bits(&before.atr), bits(&after.atr));
        assert_eq!(before.state, after.state);
        for k in 0..3 {
            assert_eq!(bits(&before.guides[k]), bits(&after.guides[k]));
        }
    }
    /// End to end through `report` (state/MB masking, guides, origin grid): each
    /// new bar that appears as the forming bar, then closes, must leave every
    /// previously reported closed-bar value bit-identical, so the drawn cloud
    /// never repaints history.
    #[test]
    fn appending_or_updating_the_forming_bar_never_revises_earlier_report_values() {
        let path: Vec<Bar> = (0..900)
            .map(|i| {
                let c = 100.0 + 15.0 * (i as f64 * 0.015).sin() + 0.8 * (i as f64 * 0.9).sin();
                Bar::new(1_700_000_000 + i as i64 * 300, c, c + 0.6, c - 0.6, c, 1.0).with_provenance(Provenance::Real)
            })
            .collect();
        let run = |b: &[Bar]| {
            report(b, Timeframe::M5, AssetClass::Crypto, None, Params::default(), Default::default()).unwrap()
        };
        let mut previous: Option<StopReport> = None;
        let mut regimes = std::collections::BTreeSet::new();
        for len in (300..path.len()).step_by(7) {
            let mut b = path[..len].to_vec();
            let r = run(&b);
            regimes.insert(r.regime);
            // A wild tick on the forming bar changes nothing.
            let last = b.last_mut().unwrap();
            last.high += 50.0;
            last.low -= 50.0;
            last.close += 40.0;
            // Debug text compares warm-up NaNs as equal, unlike `PartialEq`.
            assert_eq!(format!("{r:?}"), format!("{:?}", run(&b)), "forming tick changed the report at len {len}");
            if let Some(p) = &previous {
                let c = p.closed;
                assert!(r.closed > c);
                assert_eq!(bits(&p.mb), bits(&r.mb[..c]));
                assert_eq!(bits(&p.atr), bits(&r.atr[..c]));
                assert_eq!(p.state, r.state[..c]);
                for k in 0..3 {
                    assert_eq!(bits(&p.guides[k]), bits(&r.guides[k][..c]), "guide {k} revised at len {len}");
                }
                if p.regime == r.regime && r.regime != 0 {
                    for (old, new) in p.horizons.iter().zip(&r.horizons) {
                        assert_eq!(old.outcomes[..], new.outcomes[..old.n], "outcomes revised at len {len}");
                    }
                }
            }
            previous = Some(r);
        }
        assert!(regimes.contains(&1) && regimes.contains(&-1), "fixture must exercise both regimes: {regimes:?}");
    }
    #[test]
    fn small_n_or_few_episodes_yields_low_n_not_a_number() {
        let b = bars(301);
        let r = odds(&b, &vec![1; b.len()], &vec![2.0; b.len()], 1, 1, [1.0; 3]);
        assert!(matches!(r.touches[0], Touch::LowN { n: 300, episodes: 1 }));
        let r = odds(&b[..18], &states(18), &[2.0; 18], 1, 1, [1.0; 3]);
        assert!(matches!(r.touches[0], Touch::LowN { n: 12, .. }));
        assert!(r.touches[0].reason().contains("n=12"));
    }
    #[test]
    fn through_when_last_close_is_beyond_the_guide() {
        let mut b = bars(100);
        b[99].close = 95.0;
        assert_eq!(built(&b, vec![1; 100]).horizons[0].touches[0], Touch::Through);
    }
    #[test]
    fn flat_regime_has_no_guides_and_no_odds() {
        let r = built(&bars(100), vec![0; 100]);
        assert!(r.guides.iter().flatten().all(|g| g.is_nan()));
        assert!(r.horizons.iter().all(|h| h.n == 0 && h.touches == [Touch::NoGuide, Touch::NoGuide, Touch::NoGuide]));
    }
    #[test]
    fn zero_atr_and_nan_bars_are_skipped_and_counted() {
        let mut b = bars(100);
        b[1].low = f64::NAN;
        b[10].high = b[10].low - 1.0;
        let r = odds(&b, &[1; 100], &vec![2.0; 100], 1, 5, [1.0; 3]);
        assert_eq!(r.skipped_bad, 3); // origin 0,5,10; none replaced
        assert_eq!(r.outcomes[0].origin, 15);
        assert_eq!(odds(&b, &[1; 100], &vec![0.0; 100], 1, 5, [1.0; 3]).n, 0);
        let r = build(
            &bars(100),
            Timeframe::M5,
            AssetClass::Crypto,
            Params::default(),
            vec![100.0; 100],
            vec![0.0; 100],
            vec![1; 100],
        );
        assert_eq!(r.horizons[0].touches[0], Touch::NoGuide);
    }
    #[test]
    fn intraday_hole_drops_the_window() {
        let mut b = bars(100);
        for b in &mut b[3..] {
            b.ts += 300;
        }
        let r = odds(&b, &[1; 100], &vec![2.0; 100], 1, 5, [1.0; 3]);
        assert_eq!(r.skipped_holes, 1);
        assert_eq!(r.outcomes[0].origin, 5);
        // An overnight cash session boundary is allowed and counted.
        let mut b = bars(100);
        for b in &mut b[3..] {
            b.ts += 86400;
        }
        let cash = horizon(
            &b,
            Timeframe::M5,
            AssetClass::Equity,
            &[1; 100],
            &vec![2.0; 100],
            1,
            5,
            [1.0; 3],
            &[1.0, 2.0, 3.0],
        );
        assert_eq!(cash.skipped_holes, 0);
        assert!(cash.crossing_sessions > 0);
    }
    #[test]
    fn default_horizons_per_interval() {
        let frames = [
            Timeframe::M1,
            Timeframe::M5,
            Timeframe::M15,
            Timeframe::M30,
            Timeframe::H1,
            Timeframe::H4,
            Timeframe::D1,
            Timeframe::W1,
            Timeframe::MN1,
        ];
        for (tf, expected) in
            frames.into_iter().zip([(15, 60), (12, 48), (8, 26), (4, 13), (7, 35), (2, 10), (5, 20), (4, 13), (3, 12)])
        {
            assert_eq!(default_horizons(tf), expected);
        }
    }
    #[test]
    fn params_validate_rejects_bad_k_and_h() {
        for ks in
            [vec![], vec![0.0], vec![3.0, 2.0], vec![1.0, 1.0], vec![1.0, 2.0, 3.0, 4.0], vec![f64::NAN], vec![6.01]]
        {
            assert!(Params { ks, ..Default::default() }.validate().is_err());
        }
        for horizons in [Some((0, 5)), Some((48, 12)), Some((5, 251)), Some((12, 12))] {
            assert!(Params { horizons, ..Default::default() }.validate().is_err());
        }
        assert!(Params { ks: vec![0.25, 6.0], horizons: Some((1, 250)) }.validate().is_ok());
    }
    #[test]
    fn sample_unverified_and_mixed_history_are_refused() {
        for provenance in [Provenance::Sample, Provenance::Missing, Provenance::Cached] {
            let mut b = bars(100);
            b[99].provenance = provenance;
            let e =
                report(&b, Timeframe::M5, AssetClass::Crypto, None, Params::default(), Default::default()).unwrap_err();
            assert!(e.contains("refused"));
            assert!(!e.chars().any(|c| c.is_ascii_digit()));
        }
    }
    #[test]
    fn key_ignores_forming_prices_and_changes_on_closed_revision_params_or_trend() {
        let mut b = bars(100);
        let p = Params::default();
        let t = AdaptiveTrendParams::default();
        let key = |b: &[Bar], p, t| StopKey::new(1, Timeframe::M5, 99, p, t, b);
        let before = key(&b, p.clone(), t);
        b[99].close *= 2.0;
        assert_eq!(before, key(&b, p.clone(), t));
        b[10].high += 1.0;
        assert_ne!(before, key(&b, p.clone(), t));
        b[10].high -= 1.0;
        assert_ne!(before, key(&b, Params { ks: vec![1.5], ..p.clone() }, t));
        assert_ne!(before, key(&b, p.clone(), AdaptiveTrendParams { atr_n: 14, ..t }));
        b[99].provenance = Provenance::Cached;
        assert_ne!(before, key(&b, p.clone(), t));
        b[99].provenance = Provenance::Sample;
        assert_ne!(before, key(&b, p, t));
    }
    #[test]
    fn wilson95_known_values() {
        assert_eq!(wilson95(0, 0), None);
        for (p, expected) in [(0.0, (0.0, 0.16113)), (0.5, (0.29929, 0.70071)), (1.0, (0.83887, 1.0))] {
            let (a, b) = wilson95((p * 20.0) as usize, 20).unwrap();
            assert!((a - expected.0).abs() < 0.0001);
            assert!((b - expected.1).abs() < 0.0001);
        }
    }
    #[test]
    fn wilson95_bounds_stay_in_unit_interval_and_contain_p() {
        for n in 1..300 {
            for hits in 0..=n {
                let p = hits as f64 / n as f64;
                let (lo, hi) = wilson95(hits, n).unwrap();
                assert!(lo >= 0.0 && hi <= 1.0 && lo <= p + 1e-15 && p <= hi + 1e-15);
            }
        }
    }
}
