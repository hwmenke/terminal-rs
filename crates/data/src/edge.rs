//! Causal, descriptive regime outcomes. A return enters only after its horizon completes.
use crate::stats::{describe, forward_returns, require_research_bars};
use tr_core::Bar;

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
    /// Absent below eight effective observations, including hit-rate and significance.
    pub stats: Option<EdgeStats>,
    pub span: Option<(i64, i64)>,
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
    EdgeCell { regime, horizon, n: d.count, n_eff: d.effective_count, low_n: d.effective_count < 30.0, stats, span }
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
        }
        completed.extend(known.into_iter().map(|(index, regime, value)| CompletedOutcome {
            index,
            horizon: h,
            regime,
            value,
        }));
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
    let flips = cell(0, 5, &values, span, Some(&base));
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
}
