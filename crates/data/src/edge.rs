//! Causal, descriptive regime outcomes. A return enters only after its horizon completes.
use crate::{
    nulls::{benjamini_hochberg, rotation_offsets, rotation_test},
    stats::{describe, forward_returns, require_research_bars},
};
use tr_core::Bar;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EdgeFold {
    pub n: usize,
    /// Conditional minus purged fold baseline; absent below eight effective outcomes.
    pub delta: Option<f64>,
    pub span: Option<(i64, i64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EdgeStats {
    pub mean: f64,
    pub median: f64,
    pub std: f64,
    pub hit_rate: f64,
    pub t: Option<f64>,
    pub ci95: (f64, f64),
    pub hit_ci95: (f64, f64),
    pub delta: Option<f64>,
    pub welch_t: Option<f64>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeCell {
    pub regime: i8,
    pub horizon: usize,
    pub n: usize,
    pub n_eff: f64,
    pub low_n: bool,
    /// Return summaries (including hit-rate and t) are absent below eight effective observations.
    pub stats: Option<EdgeStats>,
    pub span: Option<(i64, i64)>,
    pub p: Option<f64>,
    pub q: Option<f64>,
    /// Percentiles of the baseline-relative rotation statistic D, not the raw mean.
    pub null_band: Option<(f64, f64)>,
    pub folds: [EdgeFold; 4],
}
impl EdgeCell {
    pub fn passes(&self) -> bool {
        let delta = self.stats.as_ref().and_then(|s| s.delta).unwrap_or(0.0);
        !self.low_n
            && self.q.is_some_and(|q| q <= 0.10)
            && delta != 0.0
            && self.folds.iter().filter(|f| f.delta.is_some_and(|d| d * delta > 0.0)).count() >= 3
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct CompletedOutcome {
    pub index: usize,
    pub horizon: usize,
    pub regime: i8,
    pub value: f64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct FlipOutcome {
    pub index: usize,
    pub direction: i8,
    pub value: Option<f64>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeReport {
    pub bars: usize,
    pub horizons: Vec<usize>,
    pub cells: Vec<EdgeCell>,
    pub baseline: Vec<EdgeCell>,
    pub flips: EdgeCell,
    pub flip_outcomes: Vec<FlipOutcome>,
    /// Auditable completed observations; appending bars cannot revise an earlier outcome.
    pub completed: Vec<CompletedOutcome>,
    pub current_regime: Option<i8>,
}
fn wilson(p: f64, n: f64) -> (f64, f64) {
    let z2 = 1.96_f64.powi(2);
    let denominator = 1.0 + z2 / n;
    let center = (p + z2 / (2.0 * n)) / denominator;
    let half = 1.96 * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denominator;
    ((center - half).max(0.0), (center + half).min(1.0))
}
fn cell(regime: i8, horizon: usize, values: &[f64], span: Option<(i64, i64)>, baseline: Option<&EdgeCell>) -> EdgeCell {
    let d = describe(values, horizon);
    let stats = (d.effective_count >= 8.0).then(|| {
        let mean = d.mean.unwrap();
        let std = d.std.unwrap_or(0.0);
        let se = std / d.effective_count.sqrt();
        let hit_rate = d.win_rate.unwrap();
        let base = baseline.and_then(|b| b.stats.as_ref().map(|s| (s, b.n_eff)));
        let welch_se = base.map(|(b, n)| (std * std / d.effective_count + b.std * b.std / n).sqrt());
        EdgeStats {
            mean,
            median: d.median.unwrap(),
            std,
            hit_rate,
            t: (se > 0.0).then(|| mean / se),
            ci95: (mean - 1.96 * se, mean + 1.96 * se),
            hit_ci95: wilson(hit_rate, d.effective_count),
            delta: base.map(|(b, _)| mean - b.mean),
            welch_t: base.zip(welch_se).and_then(|((b, _), se)| (se > 0.0).then(|| (mean - b.mean) / se)),
        }
    });
    EdgeCell {
        regime,
        horizon,
        n: d.count,
        n_eff: d.effective_count,
        low_n: d.effective_count < 30.0,
        stats,
        span,
        p: None,
        q: None,
        null_band: None,
        folds: Default::default(),
    }
}

struct HorizonData {
    h: usize,
    known: Vec<(usize, i8, f64)>,
    mean: Option<f64>,
}
fn average(sum: f64, n: usize) -> Option<f64> {
    (n > 0).then(|| sum / n as f64)
}

fn purged_folds(data: &HorizonData, bars: &[Bar], regime: i8, flips: Option<(&[bool], &[bool])>) -> [EdgeFold; 4] {
    std::array::from_fn(|fold| {
        let start = data.known.len() * fold / 4;
        let end = data.known.len() * (fold + 1) / 4;
        if start == end {
            return EdgeFold::default();
        }
        let edge = data.known.get(end).map(|v| v.0).unwrap_or_else(|| data.known[end - 1].0 + 1);
        let mut sum = 0.0;
        let mut n = 0;
        let mut base_sum = 0.0;
        let mut base_n = 0;
        let mut span = None;
        for &(i, state, r) in &data.known[start..end] {
            if i + data.h >= edge {
                continue;
            }
            base_sum += r;
            base_n += 1;
            let value = if let Some((long, short)) = flips {
                (long[i] || short[i]).then_some(r * if long[i] { 1.0 } else { -1.0 })
            } else {
                (state == regime).then_some(r)
            };
            if let Some(value) = value {
                sum += value;
                n += 1;
                let first = span.map(|s: (i64, i64)| s.0).unwrap_or(bars[i].ts);
                span = Some((first, bars[i + data.h].ts));
            }
        }
        EdgeFold {
            n,
            delta: (n as f64 / data.h as f64 >= 8.0).then(|| sum / n as f64 - base_sum / base_n as f64),
            span,
        }
    })
}

/// `None` state is a warm-up bar (non-finite TrendStack confidence). States and entry
/// flags must be known at each bar's close. Callers supply closed bars only.
pub fn regime_edge(
    bars: &[Bar],
    state: &[Option<i8>],
    entry_long: &[bool],
    entry_short: &[bool],
    horizons: &[usize],
) -> Result<EdgeReport, String> {
    const OP: &str = "TSTACK Edge";
    require_research_bars(bars, OP)?;
    Bar::provenance(bars).require_verified(OP)?;
    if bars.iter().any(|b| b.provenance != bars[0].provenance) {
        return Err(format!("{OP} refused: mixed real/cache history"));
    }
    if [state.len(), entry_long.len(), entry_short.len()].iter().any(|n| *n != bars.len())
        || state.iter().flatten().any(|s| !(-1..=1).contains(s))
        || entry_long.iter().zip(entry_short).any(|(a, b)| *a && *b)
    {
        return Err(format!("{OP} refused: misaligned or invalid causal states"));
    }
    let horizons = if horizons.is_empty() { vec![1, 5, 20] } else { horizons.to_vec() };
    if horizons.contains(&0) {
        return Err(format!("{OP} refused: horizon must be positive"));
    }
    let closes: Vec<_> = bars.iter().map(|b| b.close).collect();
    let mut cells = Vec::new();
    let mut baseline = Vec::new();
    let mut completed = Vec::new();
    let mut data = Vec::new();
    let mut observed = Vec::new();
    for &h in &horizons {
        let fwd = forward_returns(&closes, h);
        let known: Vec<_> = state.iter().enumerate().filter_map(|(i, s)| Some((i, (*s)?, fwd[i]?))).collect();
        let all: Vec<_> = known.iter().map(|(_, _, r)| *r).collect();
        let span = known.first().zip(known.last()).map(|(a, b)| (bars[a.0].ts, bars[b.0 + h].ts));
        let base = cell(0, h, &all, span, None);
        for regime in [1, 0, -1] {
            let selected: Vec<_> = known.iter().filter(|(_, s, _)| *s == regime).collect();
            let values: Vec<_> = selected.iter().map(|(_, _, r)| *r).collect();
            let span = selected.first().zip(selected.last()).map(|(a, b)| (bars[a.0].ts, bars[b.0 + h].ts));
            cells.push(cell(regime, h, &values, span, Some(&base)));
            observed.push(
                average(values.iter().sum(), values.len())
                    .zip(average(all.iter().sum(), all.len()))
                    .map(|(a, b)| a - b),
            );
        }
        completed.extend(known.iter().map(|&(index, regime, value)| CompletedOutcome {
            index,
            horizon: h,
            regime,
            value,
        }));
        data.push(HorizonData { h, known, mean: average(all.iter().sum(), all.len()) });
        baseline.push(base);
    }
    let flip_returns = forward_returns(&closes, 5);
    let flip_outcomes: Vec<_> = (0..bars.len())
        .filter(|&i| state[i].is_some() && (entry_long[i] || entry_short[i]))
        .map(|i| {
            let direction = if entry_long[i] { 1 } else { -1 };
            FlipOutcome { index: i, direction, value: flip_returns[i].map(|r| r * direction as f64) }
        })
        .collect();
    let values: Vec<_> = flip_outcomes.iter().filter_map(|f| f.value).collect();
    // Compare signed flip outcomes to the unconditional five-bar return baseline.
    let base_values: Vec<_> =
        forward_returns(&closes, 5).into_iter().enumerate().filter_map(|(i, r)| state[i].and(r)).collect();
    let base = cell(0, 5, &base_values, None, None);
    let span = flip_outcomes
        .iter()
        .filter(|f| f.value.is_some())
        .next()
        .zip(flip_outcomes.iter().rev().find(|f| f.value.is_some()))
        .map(|(a, b)| (bars[a.index].ts, bars[b.index + 5].ts));
    let mut flips = cell(0, 5, &values, span, Some(&base));
    observed.push(
        average(values.iter().sum(), values.len())
            .zip(average(base_values.iter().sum(), base_values.len()))
            .map(|(a, b)| a - b),
    );
    if !horizons.contains(&5) {
        let known = state.iter().enumerate().filter_map(|(i, s)| Some((i, (*s)?, flip_returns[i]?))).collect();
        data.push(HorizonData { h: 5, known, mean: average(base_values.iter().sum(), base_values.len()) });
    }
    for (col, horizon) in data.iter().take(horizons.len()).enumerate() {
        for row in 0..3 {
            let c = &mut cells[col * 3 + row];
            c.folds = purged_folds(horizon, bars, c.regime, None);
        }
    }
    flips.folds = purged_folds(data.iter().find(|d| d.h == 5).unwrap(), bars, 0, Some((entry_long, entry_short)));
    let warm_start = state.iter().position(Option::is_some).unwrap_or(bars.len());
    let warm_len = bars.len() - warm_start;
    let offsets = rotation_offsets(
        bars.len(),
        bars[0].ts,
        bars.last().unwrap().ts,
        warm_len,
        *horizons.iter().max().unwrap().max(&5),
        data.len(),
    );
    let nulls = rotation_test(&observed, &offsets, |offset| {
        let mut ds = vec![None; observed.len()];
        for (col, horizon) in data.iter().enumerate() {
            let mut sums = [0.0; 3];
            let mut counts = [0; 3];
            let mut flip_sum = 0.0;
            let mut flip_n = 0;
            for &(i, _, r) in &horizon.known {
                // A single source index rotates state AND both entry flags. The
                // final h signals remain in the block even without an outcome.
                let source = warm_start + (i - warm_start + offset) % warm_len;
                if let Some(state) = state[source] {
                    let row = (1 - state) as usize;
                    sums[row] += r;
                    counts[row] += 1;
                    if horizon.h == 5 && (entry_long[source] || entry_short[source]) {
                        flip_sum += r * if entry_long[source] { 1.0 } else { -1.0 };
                        flip_n += 1;
                    }
                }
            }
            if col < horizons.len() {
                for row in 0..3 {
                    ds[col * 3 + row] = average(sums[row], counts[row]).zip(horizon.mean).map(|(a, b)| a - b);
                }
            }
            if horizon.h == 5 {
                *ds.last_mut().unwrap() = average(flip_sum, flip_n).zip(horizon.mean).map(|(a, b)| a - b);
            }
        }
        ds
    });
    for (c, null) in cells.iter_mut().chain(std::iter::once(&mut flips)).zip(nulls) {
        c.p = null.p;
        c.null_band = null.band;
    }
    let ps: Vec<_> = cells.iter().chain(std::iter::once(&flips)).filter_map(|c| c.p).collect();
    let mut qs = benjamini_hochberg(&ps).into_iter();
    for c in cells.iter_mut().chain(std::iter::once(&mut flips)) {
        if c.p.is_some() {
            c.q = qs.next();
        }
    }
    Ok(EdgeReport {
        bars: bars.len(),
        horizons,
        cells,
        baseline,
        flips,
        flip_outcomes,
        completed,
        current_regime: state.last().copied().flatten(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::{
        Provenance, Timeframe,
        indicators::{AdaptiveTrendParams, trend_stack},
    };
    fn bars(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let c = 100.0 + i as f64 * 0.05 + (i as f64 * 0.37).sin();
                Bar::new(i as i64 * 86400, c, c + 1.0, c - 1.0, c, 1.0).with_provenance(Provenance::Real)
            })
            .collect()
    }
    fn report(b: &[Bar]) -> EdgeReport {
        let s = trend_stack(b, Timeframe::D1, vec![], AdaptiveTrendParams::default()).unwrap();
        let state: Vec<_> = s.medium.iter().zip(&s.confidence).map(|(&s, c)| c.is_finite().then_some(s)).collect();
        regime_edge(b, &state, &s.entry_long, &s.entry_short, &[1, 5, 20]).unwrap()
    }
    #[test]
    fn edge_is_lookahead_free() {
        let b = bars(400);
        let short = report(&b[..250]);
        for n in [251, 270, 400] {
            let full = report(&b[..n]);
            for outcome in &short.completed {
                assert_eq!(
                    Some(outcome),
                    full.completed.iter().find(|o| o.index == outcome.index && o.horizon == outcome.horizon)
                );
            }
            for h in [1, 5, 20] {
                assert_eq!(
                    short.completed.iter().filter(|o| o.horizon == h).count(),
                    full.completed.iter().filter(|o| o.horizon == h && o.index + h < 250).count()
                );
            }
            for flip in short.flip_outcomes.iter().filter(|f| f.value.is_some()) {
                assert_eq!(Some(flip), full.flip_outcomes.iter().find(|f| f.index == flip.index));
            }
        }
    }
    #[test]
    fn edge_excludes_last_h_bars() {
        let b = bars(100);
        let mut state = vec![Some(1); 100];
        state[..10].fill(None);
        let r = regime_edge(&b, &state, &vec![false; 100], &vec![false; 100], &[1, 5, 20]).unwrap();
        for h in [1, 5, 20] {
            assert_eq!(r.cells.iter().filter(|c| c.horizon == h).map(|c| c.n).sum::<usize>(), 100 - h - 10);
        }
    }
    #[test]
    fn edge_known_value_fixture() {
        let b: Vec<_> = (1..=12)
            .map(|i| {
                let c = i as f64;
                Bar::new(i, c, c, c, c, 1.0).with_provenance(Provenance::Real)
            })
            .collect();
        let r = regime_edge(&b, &[Some(1); 12], &[false; 12], &[false; 12], &[1]).unwrap();
        let c = &r.cells[0];
        let s = c.stats.as_ref().unwrap();
        assert_eq!(c.n, 11);
        assert_eq!(c.n_eff, 11.0);
        assert!((s.mean - 0.27453430407975865).abs() < 1e-12);
        assert!((s.median - 1.0 / 6.0).abs() < 1e-12);
        assert_eq!(s.hit_rate, 1.0);
        assert!((s.std - 0.2699948645241844).abs() < 1e-12);
        assert!((s.t.unwrap() - 3.372387398251184).abs() < 1e-12);
        assert!(s.hit_ci95.0 > 0.7 && s.hit_ci95.1 <= 1.0);
        assert_eq!(s.delta, Some(0.0));
        assert_eq!(s.welch_t, Some(0.0));
    }
    #[test]
    fn edge_small_n_suppresses_stats() {
        for n in [7, 8, 29, 30] {
            let b = bars(n + 1);
            let r = regime_edge(&b, &vec![Some(1); n + 1], &vec![false; n + 1], &vec![false; n + 1], &[1]).unwrap();
            let c = &r.cells[0];
            assert_eq!(c.stats.is_some(), n >= 8);
            assert_eq!(c.low_n, n < 30);
        }
    }
    #[test]
    fn edge_refuses_sample_and_unverified() {
        assert!(regime_edge(&[], &[], &[], &[], &[]).is_err());
        for p in [Provenance::Sample, Provenance::Missing, Provenance::Cached] {
            let mut b = bars(40);
            b[0].provenance = p;
            assert!(regime_edge(&b, &[Some(1); 40], &[false; 40], &[false; 40], &[1]).is_err());
        }
        assert!(regime_edge(&bars(40), &[], &[], &[], &[1]).is_err());
    }
    #[test]
    fn flip_outcomes_pending_within_horizon() {
        let b = bars(100);
        let mut longs = vec![false; 100];
        let mut shorts = vec![false; 100];
        longs[10] = true;
        shorts[20] = true;
        for i in 95..100 {
            longs[i] = true;
        }
        let r = regime_edge(&b, &[Some(1); 100], &longs, &shorts, &[1]).unwrap();
        assert_eq!(r.flips.n, 2);
        assert_eq!(r.flip_outcomes.iter().filter(|f| f.value.is_none()).count(), 5);
        assert_eq!(r.flip_outcomes[1].value, Some(-(b[25].close / b[20].close - 1.0)));
    }

    fn walk(seed: u64, n: usize, planted: bool) -> (Vec<Bar>, Vec<Option<i8>>, Vec<bool>, Vec<bool>) {
        let mut rng = crate::nulls::SplitMix64(seed);
        let mut price = 100.0;
        let mut state = 0;
        let mut bars = Vec::new();
        let mut states = Vec::new();
        let mut longs = Vec::new();
        let mut shorts = Vec::new();
        for i in 0..n {
            let noise = (rng.next() as f64 / u64::MAX as f64 - 0.5) * 0.008;
            // Return i-1 -> i depends only on the state already known at i-1.
            price *= 1.0 + noise + if planted && state == 1 { 0.004 } else { 0.0 };
            let previous = state;
            if rng.next() % 16 == 0 {
                state = (rng.next() % 3) as i8 - 1;
            }
            bars.push(Bar::new(i as i64 * 86400, price, price, price, price, 1.0).with_provenance(Provenance::Real));
            states.push((i >= 20).then_some(state));
            longs.push(state == 1 && previous != 1);
            shorts.push(state == -1 && previous != -1);
        }
        (bars, states, longs, shorts)
    }

    #[test]
    fn rotation_null_calibrates_on_two_hundred_independent_sticky_walks() {
        let mut passes = 0;
        let mut tested = 0;
        for seed in 0..200 {
            let (b, s, l, short) = walk(0x1234_5678 + seed, 420, false);
            let r = regime_edge(&b, &s, &l, &short, &[1, 5, 20]).unwrap();
            for c in &r.cells {
                if let Some(q) = c.q {
                    tested += 1;
                    passes += usize::from(q <= 0.10);
                }
            }
        }
        assert!(tested >= 1700, "only {tested} cells tested");
        assert!(passes as f64 / tested as f64 <= 0.10, "{passes}/{tested} null cells passed BH");
    }

    #[test]
    fn planted_regime_drift_is_detected_and_report_is_deterministic() {
        let (b, s, l, short) = walk(712, 1600, true);
        let r = regime_edge(&b, &s, &l, &short, &[1, 5, 20]).unwrap();
        assert_eq!(r, regime_edge(&b, &s, &l, &short, &[1, 5, 20]).unwrap());
        let up = &r.cells[0];
        assert!(up.passes(), "planted UP: {up:?}");
        assert!(up.q.unwrap() <= 0.10);
        assert!(up.folds.iter().filter(|f| f.delta.is_some_and(|d| d > 0.0)).count() >= 3);
        let ps: Vec<_> = r.cells.iter().chain(std::iter::once(&r.flips)).filter_map(|c| c.p).collect();
        let qs: Vec<_> = r.cells.iter().chain(std::iter::once(&r.flips)).filter_map(|c| c.q).collect();
        assert_eq!(qs, benjamini_hochberg(&ps));
    }

    #[test]
    fn rotation_matches_direct_whole_block_reference_including_entry_flags() {
        let (b, s, l, short) = walk(912, 260, false);
        let r = regime_edge(&b, &s, &l, &short, &[1, 5, 20]).unwrap();
        let w = b.len() - 20;
        let returns = |h: usize| forward_returns(&b.iter().map(|b| b.close).collect::<Vec<_>>(), h);
        let mut up_null = Vec::new();
        let mut flip_null = Vec::new();
        for k in 40..=w - 40 {
            let fwd = returns(1);
            let base: Vec<_> = (20..b.len()).filter_map(|i| fwd[i]).collect();
            let selected: Vec<_> =
                (20..b.len()).filter(|&i| s[20 + (i - 20 + k) % w] == Some(1)).filter_map(|i| fwd[i]).collect();
            if !selected.is_empty() {
                up_null.push(
                    selected.iter().sum::<f64>() / selected.len() as f64 - base.iter().sum::<f64>() / base.len() as f64,
                );
            }
            let fwd = returns(5);
            let base: Vec<_> = (20..b.len()).filter_map(|i| fwd[i]).collect();
            let selected: Vec<_> = (20..b.len())
                .filter_map(|i| {
                    let source = 20 + (i - 20 + k) % w;
                    (l[source] || short[source])
                        .then(|| fwd[i].map(|v| v * if l[source] { 1.0 } else { -1.0 }))
                        .flatten()
                })
                .collect();
            if !selected.is_empty() {
                flip_null.push(
                    selected.iter().sum::<f64>() / selected.len() as f64 - base.iter().sum::<f64>() / base.len() as f64,
                );
            }
        }
        for (c, values) in [(&r.cells[0], up_null), (&r.flips, flip_null)] {
            let observed = if c.horizon == 5 {
                let signed: Vec<_> = r.flip_outcomes.iter().filter_map(|f| f.value).collect();
                let base: Vec<_> = returns(5).into_iter().enumerate().filter_map(|(i, v)| s[i].and(v)).collect();
                signed.iter().sum::<f64>() / signed.len() as f64 - base.iter().sum::<f64>() / base.len() as f64
            } else {
                c.stats.as_ref().unwrap().delta.unwrap()
            };
            let p =
                (1 + values.iter().filter(|v| v.abs() >= observed.abs()).count()) as f64 / (values.len() + 1) as f64;
            assert_eq!(c.p, Some(p));
            let mut values = values;
            values.sort_by(f64::total_cmp);
            let percentile = |p: f64| {
                let at = (values.len() - 1) as f64 * p;
                values[at.floor() as usize] * (1.0 - at.fract()) + values[at.ceil() as usize] * at.fract()
            };
            let band = c.null_band.unwrap();
            assert!((band.0 - percentile(0.025)).abs() < 1e-12);
            assert!((band.1 - percentile(0.975)).abs() < 1e-12);
        }
    }

    #[test]
    fn folds_have_equal_entry_counts_then_purge_crossing_outcomes() {
        let b = bars(200);
        let data = HorizonData { h: 5, known: (0..195).map(|i| (i, 1, i as f64)).collect(), mean: None };
        let folds = purged_folds(&data, &b, 1, None);
        assert_eq!(folds.each_ref().map(|f| f.n), [43, 44, 44, 44]);
        for (fold, edge) in folds.iter().zip([48, 97, 146, 195]) {
            assert_eq!(fold.delta, Some(0.0));
            assert!(fold.span.unwrap().1 < b[edge].ts);
        }
        let small = HorizonData { h: 5, known: (0..160).map(|i| (i, 1, 0.0)).collect(), mean: None };
        assert!(purged_folds(&small, &b, 1, None).iter().all(|f| f.n == 35 && f.delta.is_none()));
    }

    #[test]
    fn solid_requires_q_sample_size_and_three_folds_in_observed_direction() {
        let (b, s, l, short) = walk(712, 1600, true);
        let mut c = regime_edge(&b, &s, &l, &short, &[1]).unwrap().cells.remove(0);
        c.q = Some(0.10);
        assert!(c.passes());
        c.q = Some(0.1001);
        assert!(!c.passes());
        c.q = None;
        assert!(!c.passes());
        c.q = Some(0.01);
        c.low_n = true;
        assert!(!c.passes());
        c.low_n = false;
        c.folds[0].delta = None;
        assert!(c.passes());
        c.folds[1].delta = Some(-1.0);
        assert!(!c.passes());
    }
}
