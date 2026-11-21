//! Parameter sensitivity of the existing Edge Proof statistic. No fetching or fallbacks.
use crate::{edge::EdgeCell, nulls::benjamini_hochberg};
use tr_core::{
    AssetClass, Bar, Provenance, Timeframe,
    indicators::{AdaptiveMethod, AdaptiveTrendParams, trend_grid},
};

pub const FDR: f64 = 0.10;
/// Upper bound on worker threads one report may occupy.
pub const MAX_THREADS: usize = 8;
pub const HORIZON: usize = 5;
pub const MAX_BARS: usize = 5_000;
pub const ER_LENGTHS: [usize; 7] = [8, 12, 16, 20, 24, 32, 40];
pub const HOLDS: [usize; 7] = [2, 4, 6, 10, 14, 20, 30];

#[derive(Debug, Clone, PartialEq)]
pub struct TuneCell {
    pub er: usize,
    pub hold: usize,
    pub edge: EdgeCell,
    pub robustness: f64,
    /// Medium-regime flips (long + short entries, i.e. trades) on common warmed bars.
    pub flips: usize,
}
impl TuneCell {
    pub fn delta(&self) -> Option<f64> {
        self.edge.stats.as_ref()?.delta
    }
    pub fn passes(&self) -> bool {
        self.edge.passes()
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct TuneReport {
    pub cells: Vec<TuneCell>,
    pub current: usize,
    /// Center of the strongest supported 3×3 neighbourhood; absent without support.
    pub plateau: Option<usize>,
    pub smoothing: Option<(f64, f64)>,
    pub regime: i8,
    pub bars: usize,
    pub eligible: usize,
    pub first_ts: i64,
    pub last_ts: i64,
    pub provenance: Provenance,
    pub source: String,
    pub elapsed_ms: f64,
    pub heat_scale_bp: f64,
}

/// Invalid/missing p values still count as trials (p=1), so the family is always 49.
fn adjust(cells: &mut [TuneCell]) {
    let ps: Vec<_> = cells.iter().map(|c| c.edge.p.unwrap_or(1.0)).collect();
    for (c, q) in cells.iter_mut().zip(benjamini_hochberg(&ps)) {
        c.edge.q = c.edge.p.map(|_| q);
    }
}
fn neighbours(i: usize, width: usize, len: usize) -> Vec<usize> {
    (0..len)
        .filter(|&j| j != i && (j / width).abs_diff(i / width) <= 1 && (j % width).abs_diff(i % width) <= 1)
        .collect()
}
fn median(values: &mut [f64]) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    (n > 0).then(|| (values[(n - 1) / 2] + values[n / 2]) / 2.0)
}
/// Support from neighbours, attenuated when their median cannot sustain the peak.
fn score(cells: &[TuneCell], i: usize, width: usize) -> f64 {
    let Some(d) = cells[i].delta().filter(|d| *d != 0.0 && cells[i].passes()) else { return 0.0 };
    let ns = neighbours(i, width, cells.len());
    let support = ns.iter().filter(|&&j| cells[j].passes() && cells[j].delta().is_some_and(|v| v * d > 0.0)).count();
    let mut values: Vec<_> = ns.iter().filter_map(|&j| cells[j].delta()).collect();
    if values.len() != ns.len() {
        return 0.0;
    }
    let attenuation = median(&mut values).map_or(0.0, |m| (m / d).clamp(0.0, 1.0));
    support as f64 / ns.len().max(1) as f64 * attenuation
}
fn plateau(cells: &mut [TuneCell]) -> Option<usize> {
    let scores: Vec<_> = (0..cells.len()).map(|i| score(cells, i, 7)).collect();
    for (c, score) in cells.iter_mut().zip(scores) {
        c.robustness = score;
    }
    // Stable ties prefer the first region in grid order, never the highest edge.
    (0..cells.len())
        .filter(|&i| cells[i].robustness >= 0.5)
        .max_by(|&a, &b| cells[a].robustness.total_cmp(&cells[b].robustness).then_with(|| b.cmp(&a)))
}

/// Exact alpha used by adaptive_ma on HLC3; convert to EMA-equivalent length 2/alpha−1.
fn smoothing(bars: &[Bar], tf: Timeframe, p: AdaptiveTrendParams) -> Option<(f64, f64)> {
    let src: Vec<_> = bars.iter().map(|b| (b.high + b.low + b.close) / 3.0).collect();
    let mut start = 0;
    let mut lengths = Vec::new();
    for i in 0..src.len() {
        if p.session_reset && tf.is_intraday() && i > 0 {
            let wall = tr_core::ny::civil(bars[i].ts);
            let open = tr_core::ny::to_utc(wall.date().and_hms_opt(9, 30, 0).unwrap());
            if wall.date() != tr_core::ny::civil(bars[i - 1].ts).date() || (bars[i - 1].ts < open && bars[i].ts >= open)
            {
                start = i;
            }
        }
        if i - start < p.mb_er {
            continue;
        }
        let vol: f64 = src[i - p.mb_er..=i].windows(2).map(|w| (w[1] - w[0]).abs()).sum();
        let er = if vol > 1e-12 { (src[i] - src[i - p.mb_er]).abs() / vol } else { 0.0 };
        let mut alpha =
            2.0 / (p.mb_slow as f64 + 1.0) + er * (2.0 / (p.mb_fast as f64 + 1.0) - 2.0 / (p.mb_slow as f64 + 1.0));
        if p.method == AdaptiveMethod::Kama {
            alpha *= alpha;
        }
        alpha = alpha.clamp(0.0, 1.0);
        if alpha > 0.0 {
            lengths.push(2.0 / alpha - 1.0);
        }
    }
    lengths.iter().copied().reduce(f64::min).zip(lengths.iter().copied().reduce(f64::max))
}

/// Full admission before trimming: even an unverified forming bar is refused.
/// Gaps in stitched history are refused rather than treated as adjacent intervals.
#[allow(clippy::too_many_arguments)]
pub fn compute(
    bars: &[Bar],
    tf: Timeframe,
    class: AssetClass,
    as_of: Option<i64>,
    p: AdaptiveTrendParams,
    source: &str,
    gaps: &[(i64, i64)],
) -> Result<TuneReport, String> {
    compute_capped(bars, tf, class, as_of, p, source, gaps, MAX_BARS)
}

#[allow(clippy::too_many_arguments)]
fn compute_capped(
    bars: &[Bar],
    tf: Timeframe,
    class: AssetClass,
    as_of: Option<i64>,
    p: AdaptiveTrendParams,
    source: &str,
    gaps: &[(i64, i64)],
    max_bars: usize,
) -> Result<TuneReport, String> {
    let start = std::time::Instant::now();
    crate::stats::require_research_bars(bars, "TSTACK Tune")?;
    if p.mb_er == 0 || p.sb_er == 0 || p.lb_er == 0 || p.atr_n == 0 || !p.confirm_mult.is_finite() {
        return Err("TSTACK Tune refused: invalid trend parameters".into());
    }
    let n = crate::closed::closed_len(tf, bars, class, as_of);
    let bars = &bars[n.saturating_sub(max_bars)..n];
    if bars.len() < 256 {
        return Err("TSTACK Tune needs at least 256 verified closed bars; no sample fallback.".into());
    }
    if gaps.iter().any(|g| g.1 > bars[0].ts && g.0 < bars.last().unwrap().ts) {
        return Err(
            "TSTACK Tune refused: UNVERIFIED JOIN in the analysis window. Load contiguous verified history.".into()
        );
    }
    // Include the captured current setting even if defaults change in the future.
    let axis = |mut values: [usize; 7], current| {
        if !values.contains(&current) {
            values[3] = current;
            values.sort_unstable();
        }
        values
    };
    let ers = axis(ER_LENGTHS, p.mb_er);
    let holds = axis(HOLDS, p.min_hold);
    let params: Vec<_> = holds
        .iter()
        .flat_map(|&min_hold| ers.iter().map(move |&mb_er| AdaptiveTrendParams { mb_er, min_hold, ..p }))
        .collect();
    // Shared baselines/ATR once, the medium baseline once per ER column, then only
    // the sticky state machine per cell (bit-identical to 49 `trend_stack` calls).
    let stacks = trend_grid(bars, tf, p, &ers, &holds)?;
    // Identical causal eligibility in every cell, including session-reset warmups.
    let eligible: Vec<_> = (0..bars.len()).map(|i| stacks.iter().all(|s| s.confidence[i].is_finite())).collect();
    let eligible_n = eligible.iter().filter(|&&v| v).count();
    if eligible_n < 200 {
        return Err("TSTACK Tune needs at least 200 common warmed closed bars across all settings.".into());
    }
    let current = params
        .iter()
        .position(|v| v.mb_er == p.mb_er && v.min_hold == p.min_hold)
        .ok_or("TSTACK Tune internal error: current setting missing from grid")?;
    if !eligible[bars.len() - 1] {
        return Err("TSTACK Tune refused: current closed bar is still warming.".into());
    }
    let regime = stacks[current].medium_state[bars.len() - 1];
    // The rotation null dominates; spread the 49 cells over the available cores
    // with scoped std threads (no rayon dependency in this crate).
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, MAX_THREADS);
    let per = stacks.len().div_ceil(threads);
    let edges = std::thread::scope(|scope| {
        let jobs: Vec<_> = stacks
            .chunks(per)
            .map(|chunk| {
                let eligible = &eligible;
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|s| {
                            let states: Vec<_> =
                                s.medium_state.iter().zip(eligible).map(|(&s, &ok)| ok.then_some(s)).collect();
                            let r = crate::edge::regime_edge(bars, &states, &s.entry_long, &s.entry_short, &[HORIZON])?;
                            r.cells
                                .into_iter()
                                .find(|c| c.regime == regime)
                                .ok_or_else(|| "TSTACK Tune internal error: regime cell missing".to_string())
                        })
                        .collect::<Result<Vec<_>, String>>()
                })
            })
            .collect();
        let mut out = Vec::new();
        for job in jobs {
            out.extend(job.join().map_err(|_| "TSTACK Tune edge task panicked")??);
        }
        Ok::<_, String>(out)
    })?;
    let mut cells: Vec<_> = params
        .iter()
        .zip(edges)
        .zip(&stacks)
        .map(|((p, edge), s)| {
            let flips = (0..bars.len()).filter(|&i| eligible[i] && (s.entry_long[i] || s.entry_short[i])).count();
            TuneCell { er: p.mb_er, hold: p.min_hold, edge, robustness: 0.0, flips }
        })
        .collect();
    adjust(&mut cells);
    let plateau = plateau(&mut cells);
    let scale = cells.iter().filter_map(TuneCell::delta).map(|d| d.abs() * 10_000.0).fold(1.0, f64::max);
    Ok(TuneReport {
        current,
        plateau,
        smoothing: smoothing(bars, tf, p),
        regime,
        bars: bars.len(),
        eligible: eligible_n,
        first_ts: bars[0].ts,
        last_ts: bars.last().unwrap().ts,
        provenance: Bar::provenance(bars),
        source: source.into(),
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        heat_scale_bp: scale,
        cells,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge::{EdgeFold, EdgeStats};
    fn bars(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let c = 100.0 + i as f64 * 0.003 + (i as f64 * 0.071).sin() * 4.0 + (i as f64 * 0.41).sin();
                Bar::new(i as i64 * 86400, c, c + 0.2, c - 0.2, c, 1000.0).with_provenance(Provenance::Real)
            })
            .collect()
    }
    fn run(b: &[Bar], at: Option<i64>) -> Result<TuneReport, String> {
        compute(b, Timeframe::D1, AssetClass::Crypto, at, AdaptiveTrendParams::default(), "test fixture", &[])
    }
    fn cell(delta: f64, pass: bool) -> TuneCell {
        let stats = EdgeStats {
            mean: delta,
            median: delta,
            std: 0.01,
            hit_rate: 0.6,
            t: Some(2.0),
            ci95: (0.0, 0.01),
            hit_ci95: (0.4, 0.8),
            delta: Some(delta),
            welch_t: Some(2.0),
        };
        TuneCell {
            er: 20,
            hold: 10,
            robustness: 0.0,
            flips: 12,
            edge: EdgeCell {
                regime: 1,
                horizon: 5,
                n: 500,
                n_eff: 100.0,
                low_n: false,
                stats: Some(stats),
                span: None,
                p: Some(0.01),
                q: Some(if pass { 0.01 } else { 1.0 }),
                null_band: None,
                folds: std::array::from_fn(|_| EdgeFold { n: 100, delta: Some(delta), span: None }),
            },
        }
    }
    #[test]
    fn fdr_is_step_up_across_the_49_cell_family() {
        // m = 49. Sorted p = .0025, .004, .006 (rest p = 1 / missing).
        // Thresholds k·0.10/49: .00204, .00408, .00612. p(1) fails its own
        // threshold but k = 3 passes, so step-up rejects all three.
        // q(3) = .006·49/3 = .098; q(2) = min(.098, .004·49/2 = .098);
        // q(1) = min(.098, .0025·49 = .1225) = .098 (monotone).
        let mut cells = vec![cell(0.001, true); 49];
        for (k, c) in cells.iter_mut().enumerate() {
            c.edge.p = if k < 40 { Some(1.0) } else { None };
        }
        for (k, p) in [(7, 0.0025), (8, 0.004), (9, 0.006)] {
            cells[k].edge.p = Some(p);
        }
        adjust(&mut cells);
        for k in [7, 8, 9] {
            assert!((cells[k].edge.q.unwrap() - 0.098).abs() < 1e-12, "cell {k}: {:?}", cells[k].edge.q);
            assert!(cells[k].passes());
        }
        assert!(cells.iter().enumerate().filter(|(k, _)| ![7, 8, 9].contains(k)).all(|(_, c)| !c.passes()));
        // p = .0025, .0045, .0065: q(3) = .10617, q(2) = min(q(3), .11025),
        // q(1) = min(q(2), .1225) → all .10617 > .10, nothing passes.
        cells[8].edge.p = Some(0.0045);
        cells[9].edge.p = Some(0.0065);
        adjust(&mut cells);
        assert!(cells.iter().all(|c| !c.passes()));
    }
    #[test]
    fn fdr_known_values_counts_all_49_including_missing_tests() {
        let mut cells = vec![cell(0.001, true); 49];
        for c in &mut cells {
            c.edge.p = None;
        }
        for (c, p) in cells.iter_mut().zip([0.001, 0.005, 0.02, 0.04]) {
            c.edge.p = Some(p);
        }
        adjust(&mut cells);
        for (c, q) in cells.iter().zip([0.049, 0.1225, 0.32666666666666666, 0.49]) {
            assert!((c.edge.q.unwrap() - q).abs() < 1e-12);
        }
        assert!(cells[4].edge.q.is_none());
    }
    #[test]
    fn plateau_beats_a_higher_lone_peak_and_ties_are_stable() {
        let mut cells = vec![cell(-0.001, false); 49];
        for y in 1..=3 {
            for x in 1..=3 {
                cells[y * 7 + x] = cell(0.002, true);
            }
        }
        cells[40] = cell(0.02, true);
        assert_eq!(plateau(&mut cells), Some(16));
        assert_eq!(cells[16].robustness, 1.0);
        assert_eq!(cells[40].robustness, 0.0);
        cells[16] = cell(0.02, true);
        assert!((score(&cells, 16, 7) - 0.1).abs() < 1e-12);
        let mut empty = vec![cell(0.001, false); 49];
        assert_eq!(plateau(&mut empty), None);
    }
    #[test]
    fn smoothing_uses_actual_kama_and_adma_alpha() {
        let b: Vec<_> = (1..=100).map(|i| Bar::new(i, i as f64, i as f64, i as f64, i as f64, 1.0)).collect();
        let p = AdaptiveTrendParams::default();
        let (lo, hi) = smoothing(&b, Timeframe::D1, p).unwrap();
        assert!((lo - 3.5).abs() < 1e-12 && (hi - lo).abs() < 1e-12);
        let (lo, hi) = smoothing(&b, Timeframe::D1, AdaptiveTrendParams { method: AdaptiveMethod::Adma, ..p }).unwrap();
        assert!((lo - 2.0).abs() < 1e-12 && (hi - lo).abs() < 1e-12);
        let flat = vec![Bar::new(0, 100.0, 100.0, 100.0, 100.0, 1.0); 100];
        let (lo, hi) = smoothing(&flat, Timeframe::D1, p).unwrap();
        assert!((lo - 1859.5).abs() < 1e-9 && lo == hi);
    }
    #[test]
    fn refuses_short_unverified_bad_and_gapped_inputs_before_trimming() {
        assert!(run(&bars(255), None).is_err());
        for provenance in [Provenance::Sample, Provenance::Missing] {
            let mut b = bars(300);
            b[299].provenance = provenance;
            assert!(run(&b, None).unwrap_err().contains("refused"));
        }
        let mut b = bars(300);
        b[299].close = f64::NAN;
        assert!(run(&b, None).is_err());
        let b = bars(300);
        assert!(
            compute(
                &b,
                Timeframe::D1,
                AssetClass::Crypto,
                None,
                AdaptiveTrendParams::default(),
                "fixture",
                &[(b[20].ts, b[21].ts)]
            )
            .unwrap_err()
            .contains("JOIN")
        );
        assert!(
            compute(
                &b,
                Timeframe::D1,
                AssetClass::Crypto,
                None,
                AdaptiveTrendParams { mb_er: 0, ..Default::default() },
                "fixture",
                &[]
            )
            .is_err()
        );
    }
    #[test]
    fn grid_is_deterministic_and_ignores_the_forming_bar() {
        let mut b = bars(420);
        let r = run(&b, None).unwrap();
        assert_eq!(r.cells.len(), 49);
        assert_eq!(r.current, 24);
        assert_eq!(r.bars, 419);
        assert!(r.cells.iter().all(|c| c.edge.regime == r.regime));
        b[419].close *= 2.0;
        b[419].high = b[419].close;
        let second = run(&b, None).unwrap();
        assert_eq!(r.cells, second.cells);
        assert_eq!(r.smoothing, second.smoothing);
        assert_eq!(r.plateau, second.plateau);
        assert!(r.cells.iter().flat_map(|c| c.edge.span).all(|(_, end)| end <= r.last_ts));
    }
    #[test]
    fn grid_timing_7_by_7_on_5000_bars() {
        let b = bars(5000);
        let at = Some(b.last().unwrap().ts + 86400);
        let r = run(&b, at).unwrap();
        assert_eq!(r.bars, 5000);
        eprintln!("TSTACK Tune 7×7 / 5,000 synthetic TEST bars: {:.1} ms ({} warmed)", r.elapsed_ms, r.eligible);
        // Timing is reported, not asserted: CI hardware and debug profiles vary.
    }
    /// `cargo test -p tr-data --release tstack_tune::tests::cold_grid_bench -- --ignored --nocapture`
    #[test]
    #[ignore = "timing benchmark"]
    fn cold_grid_bench() {
        for n in [5_000, 20_000] {
            let b = bars(n);
            let at = Some(b.last().unwrap().ts + 86400);
            let mut best = f64::INFINITY;
            for _ in 0..3 {
                let t = std::time::Instant::now();
                let r = compute_capped(&b, Timeframe::D1, AssetClass::Crypto, at, Default::default(), "bench", &[], n)
                    .unwrap();
                assert_eq!(r.bars, n);
                best = best.min(t.elapsed().as_secs_f64() * 1000.0);
            }
            eprintln!("BENCH cold 7x7 grid on {n} synthetic bars: {best:.1} ms (best of 3)");
        }
    }
}
