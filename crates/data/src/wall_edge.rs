//! Wall-wide evidence over verified, completed bars. Acquisition belongs to the worker.
use crate::{
    edge::{EdgeCell, regime_edge},
    stack_wall::{self, Row, Sort},
    vault::StitchInfo,
};
use chrono::{NaiveDate, Timelike};
use tr_core::{
    AssetClass, Bar, Provenance, Timeframe,
    indicators::{AdaptiveTrendParams, trend_stack},
};

pub const HORIZON: usize = 5;
pub const MAX_BARS: usize = 5_000;
pub const MIN_BARS: usize = 256;
#[derive(Debug, Clone, PartialEq)]
pub struct Miss {
    pub code: &'static str,
    pub detail: String,
    pub is_test: bool,
}
impl Miss {
    pub fn new(code: &'static str, detail: impl Into<String>, is_test: bool) -> Self {
        Self { code, detail: detail.into(), is_test }
    }
    pub fn unverified(detail: impl Into<String>) -> Self {
        Self::new("UNVERIFIED", detail, false)
    }
}
#[derive(Debug, Clone)]
pub struct WallEdge {
    pub cells: [EdgeCell; 3],
    pub as_of_regime: Option<i8>,
    pub baseline_mean: Option<f64>,
    pub bars: usize,
    pub first_ts: i64,
    pub closed_thru_ts: i64,
    pub tf: Timeframe,
    pub provenance: Provenance,
    pub source: String,
    pub vault_bars: usize,
    pub live_bars: usize,
    pub trimmed_gap: bool,
    pub vault_only: bool,
}
impl WallEdge {
    pub fn cell(&self, regime: Option<i8>) -> Option<&EdgeCell> {
        let regime = regime.or(self.as_of_regime)?;
        self.cells.iter().find(|c| c.regime == regime)
    }
}
/// Session identity rolls exactly at the cash close, including half days and DST.
pub fn completed_session(now: i64) -> NaiveDate {
    let wall = tr_core::ny::civil(now);
    let d = wall.date();
    if crate::history::calendar::is_session(d) && wall.hour() * 60 + wall.minute() >= crate::intraday::close_minute(d) {
        d
    } else {
        crate::history::calendar::previous_session(d)
    }
}
/// A vault-only series has no observation clock: conservatively drop its last date.
pub fn session_cut(bars: &[Bar], observed: Option<i64>) -> usize {
    let Some(last) = bars.last() else { return 0 };
    let d = tr_core::ny::civil(last.ts).date();
    let close = crate::intraday::close_minute(d);
    let end = tr_core::ny::to_utc(d.and_hms_opt(close / 60, close % 60, 0).unwrap());
    if observed.is_some_and(|t| t >= end) {
        bars.len()
    } else {
        bars.partition_point(|b| tr_core::ny::civil(b.ts).date() < d)
    }
}
pub struct Prepared {
    pub bars: Vec<Bar>,
    pub trimmed_gap: bool,
}
/// Validate before filtering so discarded bars cannot hide bad provenance.
pub fn prepare(
    bars: &[Bar],
    tf: Timeframe,
    class: AssetClass,
    observed: Option<i64>,
    stitch: Option<&StitchInfo>,
) -> Result<Prepared, Miss> {
    if stitch.and_then(|s| s.note.as_ref()).is_some_and(|n| n.contains("QUARANTINED")) {
        return Err(Miss::new("QUARANTINED", "Fresh fetch withheld by the Bar Vault", false));
    }
    if bars.is_empty() {
        return Err(Miss::new("NO DATA", "No verified history available", false));
    }
    crate::stats::require_research_bars(bars, "Wall Edge").map_err(Miss::unverified)?;
    let bars = crate::vault::research_bars(bars, stitch).map_err(Miss::unverified)?;
    let bars = if tf.is_intraday() { crate::intraday::regular_only(bars.into_owned()) } else { bars.into_owned() };
    let n = crate::closed::closed_len(tf, &bars, class, observed);
    let n = if tf.is_intraday() { session_cut(&bars[..n], observed) } else { n };
    let start = n.saturating_sub(MAX_BARS);
    let gap_start = stitch
        .into_iter()
        .flat_map(|s| &s.gaps)
        .filter(|(a, b)| {
            *a < bars.get(n.saturating_sub(1)).map_or(i64::MIN, |v| v.ts)
                && *b > bars.get(start).map_or(i64::MAX, |v| v.ts)
        })
        .map(|(_, b)| bars[..n].partition_point(|v| v.ts < *b))
        .max()
        .unwrap_or(start)
        .max(start);
    let trimmed_gap = gap_start > start;
    let input = &bars[gap_start..n];
    if input.len() < MIN_BARS {
        return Err(Miss::new(
            "SHORT HISTORY",
            format!(
                "{} closed bars after session cut{}; need {MIN_BARS}",
                input.len(),
                if trimmed_gap { " and gap trim" } else { "" }
            ),
            true,
        ));
    }
    Ok(Prepared { bars: input.to_vec(), trimmed_gap })
}
pub fn compute(bars: &[Bar], tf: Timeframe, params: AdaptiveTrendParams) -> Result<WallEdge, String> {
    let stack = trend_stack(bars, tf, vec![], params)?;
    let state: Vec<_> = stack.medium.iter().zip(&stack.confidence).map(|(&s, c)| c.is_finite().then_some(s)).collect();
    let report = regime_edge(bars, &state, &stack.entry_long, &stack.entry_short, &[HORIZON])?;
    Ok(WallEdge {
        cells: report.cells.try_into().map_err(|_| "Wall Edge: expected three regimes")?,
        as_of_regime: state.last().copied().flatten(),
        baseline_mean: report.baseline[0].stats.as_ref().map(|s| s.mean),
        bars: bars.len(),
        first_ts: bars[0].ts,
        closed_thru_ts: bars.last().unwrap().ts,
        tf,
        provenance: Bar::provenance(bars),
        source: String::new(),
        vault_bars: 0,
        live_bars: 0,
        trimmed_gap: false,
        vault_only: false,
    })
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trial {
    Pending,
    NoTest,
    P(Option<f64>),
}
fn probability(p: f64) -> bool {
    p.is_finite() && (0.0..=1.0).contains(&p)
}
pub fn adjust(trials: &[Trial]) -> Vec<Option<f64>> {
    let ps: Vec<_> = trials
        .iter()
        .filter_map(|t| match t {
            Trial::NoTest => None,
            Trial::P(Some(p)) if probability(*p) => Some(*p),
            _ => Some(1.0),
        })
        .collect();
    let qs = crate::nulls::benjamini_hochberg(&ps);
    let mut i = 0;
    trials
        .iter()
        .map(|t| {
            if *t == Trial::NoTest {
                return None;
            }
            let q = qs[i];
            i += 1;
            matches!(t, Trial::P(Some(p)) if probability(*p)).then_some(q)
        })
        .collect()
}
pub fn delta(cell: &EdgeCell) -> Option<f64> {
    cell.stats.as_ref()?.delta.filter(|v| v.is_finite())
}
pub fn agrees(cell: &EdgeCell) -> usize {
    delta(cell)
        .map_or(0, |d| cell.folds.iter().filter(|f| f.delta.is_some_and(|v| v.is_finite() && v * d > 0.0)).count())
}
pub fn score(cell: &EdgeCell, q: Option<f64>) -> Option<f64> {
    let d = delta(cell)?;
    let q = q.filter(|q| probability(*q))?;
    if cell.low_n || d == 0.0 {
        return None;
    }
    Some(100.0 * d.signum() * (-q.max(0.001).log10()).min(3.0) / 3.0 * agrees(cell) as f64 / 4.0)
}
pub fn passes(cell: &EdgeCell, q: Option<f64>) -> bool {
    if delta(cell).is_none() || !q.is_some_and(probability) {
        return false;
    }
    let mut c = cell.clone();
    c.q = q;
    c.passes()
}
/// Cached display selection; UI paints these values without calculating statistics.
#[derive(Debug, Clone)]
pub struct DisplayEdge {
    pub trial: Trial,
    pub cell: Option<EdgeCell>,
    pub q: Option<f64>,
    pub score: Option<f64>,
    pub pass: bool,
    pub live: bool,
    pub miss: Option<Miss>,
}
impl DisplayEdge {
    pub fn pending() -> Self {
        Self { trial: Trial::Pending, cell: None, q: None, score: None, pass: false, live: false, miss: None }
    }
    pub fn select(report: Result<&WallEdge, &Miss>, live: Option<i8>) -> Self {
        let mut out = Self::pending();
        out.live = live.is_some();
        match report {
            Err(m) => {
                out.trial = if m.is_test { Trial::P(None) } else { Trial::NoTest };
                out.miss = Some(m.clone());
            }
            Ok(r) => {
                out.cell = r.cell(live).cloned();
                let valid = out.cell.as_ref().filter(|c| delta(c).is_some() && c.p.is_some_and(probability));
                out.trial = Trial::P(valid.and_then(|c| c.p));
                if valid.is_none() {
                    out.miss = Some(Miss::new(
                        "LOW N",
                        out.cell.as_ref().map(|c| format!("Displayed regime: n {} → n_eff {:.1}; insufficient finite outcomes or null samples", c.n, c.n_eff))
                            .unwrap_or_else(|| "No completed regime at the window end".into()),
                        true,
                    ));
                }
            }
        }
        out
    }
    pub fn delta_bp(&self) -> Option<f64> {
        if self.miss.is_some() {
            None
        } else {
            self.cell.as_ref().and_then(delta).map(|d| d * 10_000.0).filter(|d| d.is_finite())
        }
    }
}
pub fn resolve(edges: &mut [DisplayEdge]) -> Summary {
    let qs = adjust(&edges.iter().map(|e| e.trial).collect::<Vec<_>>());
    for (e, q) in edges.iter_mut().zip(qs) {
        e.q = q;
        e.score = e.cell.as_ref().and_then(|c| score(c, q));
        e.pass = e.cell.as_ref().is_some_and(|c| passes(c, q));
    }
    summary(edges)
}
pub fn order(rows: &[Row], edges: &[DisplayEdge], sort: Sort) -> Vec<usize> {
    if matches!(sort, Sort::Stack | Sort::Flip | Sort::Cushion | Sort::List) {
        return stack_wall::sorted_indices(rows, sort);
    }
    let mut ids: Vec<_> = (0..rows.len()).collect();
    let value = |i: usize| {
        edges
            .get(i)
            .and_then(|e| {
                let delta = e.delta_bp()?;
                match sort {
                    Sort::EdgeScore => e.score.map(f64::abs),
                    Sort::EdgeDelta => Some(delta),
                    _ => e.q,
                }
            })
            .filter(|v| v.is_finite())
    };
    ids.sort_by(|&a, &b| match (value(a), value(b)) {
        (Some(x), Some(y)) => (if sort == Sort::EdgeQ { x.total_cmp(&y) } else { y.total_cmp(&x) })
            .then_with(|| {
                if sort == Sort::EdgeScore {
                    edges[a].q.unwrap_or(1.0).total_cmp(&edges[b].q.unwrap_or(1.0))
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| edges[b].delta_bp().unwrap_or(0.0).abs().total_cmp(&edges[a].delta_bp().unwrap_or(0.0).abs()))
            .then(a.cmp(&b)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => rows[b].scored().cmp(&rows[a].scored()).then(a.cmp(&b)),
    });
    ids
}
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub m: usize,
    pub resolved: usize,
    pub pending: usize,
    pub pass_up: usize,
    pub pass_down: usize,
    pub median_bp: Option<f64>,
    pub q1_bp: Option<f64>,
    pub q3_bp: Option<f64>,
    pub scale_bp: f64,
    pub small: bool,
    pub lone_p: Option<f64>,
}
pub fn summary(edges: &[DisplayEdge]) -> Summary {
    let mut values: Vec<_> = edges.iter().filter_map(DisplayEdge::delta_bp).collect();
    values.sort_by(f64::total_cmp);
    let quantile = |p: f64| {
        if values.is_empty() {
            return None;
        }
        let i = (values.len() - 1) as f64 * p;
        let lo = i.floor() as usize;
        let hi = i.ceil() as usize;
        Some(values[lo] + (values[hi] - values[lo]) * i.fract())
    };
    let m = edges.iter().filter(|e| e.trial != Trial::NoTest).count();
    Summary {
        m,
        resolved: edges.iter().filter(|e| e.trial != Trial::Pending).count(),
        pending: edges.iter().filter(|e| e.trial == Trial::Pending).count(),
        pass_up: edges.iter().filter(|e| e.pass && e.delta_bp().is_some_and(|v| v > 0.0)).count(),
        pass_down: edges.iter().filter(|e| e.pass && e.delta_bp().is_some_and(|v| v < 0.0)).count(),
        median_bp: quantile(0.5),
        q1_bp: quantile(0.25),
        q3_bp: quantile(0.75),
        scale_bp: values.iter().map(|v| v.abs()).fold(1.0, f64::max),
        small: m < 10,
        lone_p: (m > 0).then(|| crate::tstack_tune::FDR / m as f64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn fixture(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let c = 100.0 + (i as f64 / 17.0).sin() * 8.0 + i as f64 * 0.01;
                Bar::new(1_700_000_000 + i as i64 * 86400, c, c + 1.0, c - 1.0, c, 1000.0)
                    .with_provenance(Provenance::Real)
            })
            .collect()
    }
    fn cell(d: f64, p: f64) -> EdgeCell {
        use crate::edge::{EdgeFold, EdgeStats};
        EdgeCell {
            regime: 1,
            horizon: 5,
            n: 400,
            n_eff: 80.0,
            low_n: false,
            stats: Some(EdgeStats {
                mean: d,
                median: d,
                std: 0.01,
                hit_rate: 0.5,
                t: None,
                ci95: (0.0, 0.0),
                hit_ci95: (0.4, 0.6),
                delta: Some(d),
                welch_t: None,
            }),
            span: None,
            p: Some(p),
            q: Some(p),
            null_band: None,
            folds: std::array::from_fn(|i| EdgeFold { n: 100, delta: Some(if i < 3 { d } else { -d }), span: None }),
        }
    }
    fn display(d: f64, p: f64) -> DisplayEdge {
        let mut e = DisplayEdge::pending();
        e.cell = Some(cell(d, p));
        e.trial = Trial::P(Some(p));
        e
    }
    fn ny(s: &str) -> i64 {
        tr_core::ny::to_utc(s.parse::<chrono::NaiveDateTime>().unwrap())
    }
    #[test]
    fn bh_runs_across_the_wall_not_per_name() {
        let trials = [
            Trial::P(Some(0.01)),
            Trial::P(Some(0.01)),
            Trial::P(Some(0.03)),
            Trial::P(Some(0.04)),
            Trial::Pending,
            Trial::Pending,
        ];
        let q = adjust(&trials);
        for (got, want) in q[..4].iter().zip([0.03, 0.03, 0.06, 0.06]) {
            assert!((got.unwrap() - want).abs() < 1e-12);
        }
        let q4 = adjust(&trials[..4]);
        for (got, want) in q4.iter().zip([0.02, 0.02, 0.04, 0.04]) {
            assert!((got.unwrap() - want).abs() < 1e-12);
        }
        assert_ne!(q[0], cell(0.01, 0.01).q);
    }
    #[test]
    fn pending_names_count_as_p_one_and_passes_are_never_retracted() {
        let c = cell(0.01, 0.02);
        for p in [0.001, 0.02, 0.5, 1.0] {
            let before = adjust(&[Trial::P(c.p), Trial::Pending]);
            let after = adjust(&[Trial::P(c.p), Trial::P(Some(p))]);
            assert!(after[0] <= before[0]);
            assert!(passes(&c, before[0]));
            assert!(passes(&c, after[0]));
        }
    }
    #[test]
    fn no_test_names_are_excluded_from_m() {
        let mut edges = vec![display(0.001, 0.01), DisplayEdge::select(Err(&Miss::unverified("bad")), None)];
        let s = resolve(&mut edges);
        assert_eq!(s.m, 1);
        assert_eq!(edges[0].q, Some(0.01));
        assert_eq!(edges[1].q, None);
    }
    #[test]
    fn short_history_and_low_n_count_as_trials_with_p_one() {
        let short = DisplayEdge::select(Err(&Miss::new("SHORT HISTORY", "12 bars", true)), None);
        let mut low = display(0.01, 0.1);
        low.trial = Trial::P(None);
        let mut edges = vec![display(0.001, 0.01), short, low];
        assert_eq!(resolve(&mut edges).m, 3);
        assert_eq!(edges[0].q, Some(0.03));
        assert_eq!(edges[1].q, None);
    }
    #[test]
    fn wall_pass_reuses_edge_cell_passes_with_wall_q() {
        let c = cell(0.01, 0.001);
        assert!(c.passes());
        assert!(!passes(&c, Some(0.2)));
        let mut copy = c.clone();
        copy.q = Some(0.08);
        assert_eq!(passes(&c, Some(0.08)), copy.passes());
    }
    #[test]
    fn score_hand_values_sign_cap_and_none_cases() {
        let mut c = cell(0.001, 0.033);
        assert!((score(&c, Some(0.033)).unwrap() - 37.0).abs() < 0.1);
        assert_eq!(score(&c, Some(1e-9)), Some(75.0));
        c.stats.as_mut().unwrap().delta = Some(-0.001);
        for f in &mut c.folds {
            f.delta = f.delta.map(|d| -d);
        }
        assert_eq!(score(&c, Some(0.0)), Some(-75.0));
        c.low_n = true;
        assert_eq!(score(&c, Some(0.03)), None);
        c.low_n = false;
        for q in [None, Some(f64::NAN), Some(-1.0), Some(1.1)] {
            assert_eq!(score(&c, q), None);
        }
        c.stats.as_mut().unwrap().delta = Some(0.0);
        assert_eq!(score(&c, Some(0.01)), None);
    }
    #[test]
    fn order_by_score_delta_q_with_ties_nan_and_missing_last() {
        let rows: Vec<_> = (0..6)
            .map(|i| Row::failure(i.to_string(), tr_core::indicators::LaneStatus::Unavailable("x".into()), false))
            .collect();
        let mut edges = vec![
            display(0.001, 0.01),
            display(-0.003, 0.001),
            display(0.002, 0.01),
            display(0.002, 0.01),
            display(f64::NAN, 0.01),
            DisplayEdge::pending(),
        ];
        resolve(&mut edges);
        assert_eq!(order(&rows, &edges, Sort::EdgeScore), vec![1, 2, 3, 0, 4, 5]);
        assert_eq!(order(&rows, &edges, Sort::EdgeDelta), vec![2, 3, 0, 1, 4, 5]);
        // Non-finite effects are missing even if their raw p is finite.
        assert_eq!(order(&rows, &edges, Sort::EdgeQ), vec![1, 2, 3, 0, 4, 5]);
    }
    #[test]
    fn stack_sorts_delegate_unchanged_to_sorted_indices() {
        let rows = vec![Row::failure("X", tr_core::indicators::LaneStatus::Unavailable("x".into()), false)];
        for sort in [Sort::Stack, Sort::Flip, Sort::Cushion, Sort::List] {
            assert_eq!(order(&rows, &[], sort), stack_wall::sorted_indices(&rows, sort));
        }
    }
    #[test]
    fn compute_matches_regime_edge_cells_for_h5() {
        let bars = fixture(900);
        let params = AdaptiveTrendParams::default();
        let stack = trend_stack(&bars, Timeframe::D1, vec![], params).unwrap();
        let state: Vec<_> =
            stack.medium.iter().zip(&stack.confidence).map(|(&s, c)| c.is_finite().then_some(s)).collect();
        let proof = regime_edge(&bars, &state, &stack.entry_long, &stack.entry_short, &[5]).unwrap();
        let wall = compute(&bars, Timeframe::D1, params).unwrap();
        assert_eq!(wall.cells.as_slice(), proof.cells);
        // Additional native lanes do not alter the base regime or confidence.
        let multi = trend_stack(&bars, Timeframe::D1, vec![(Timeframe::W1, Ok(Timeframe::W1.resample(&bars)))], params)
            .unwrap();
        assert_eq!(multi.medium, stack.medium);
        assert!(multi.confidence.iter().zip(stack.confidence).all(|(a, b)| a.to_bits() == b.to_bits()));
    }
    #[test]
    fn compute_refuses_sample_unverified_and_mixed_bars() {
        for provenance in [Provenance::Sample, Provenance::Missing, Provenance::Cached] {
            let mut bars = fixture(500);
            bars[0] = bars[0].with_provenance(provenance);
            assert!(compute(&bars, Timeframe::D1, Default::default()).is_err());
            assert!(prepare(&bars, Timeframe::D1, AssetClass::Equity, None, None).is_err());
        }
    }
    fn intrabars(dates: &[&str]) -> Vec<Bar> {
        dates
            .iter()
            .flat_map(|d| {
                let d: NaiveDate = d.parse().unwrap();
                crate::intraday::regular_opens(d, Timeframe::M5)
                    .into_iter()
                    .map(|ts| Bar::new(ts, 100.0, 101.0, 99.0, 100.0, 1.0).with_provenance(Provenance::Real))
            })
            .collect()
    }
    #[test]
    fn session_cut_drops_an_unfinished_session_and_keeps_a_finished_one() {
        let bars = intrabars(&["2026-11-12", "2026-11-13"]);
        assert_eq!(session_cut(&bars, Some(ny("2026-11-13T15:59:59"))), 78);
        assert_eq!(session_cut(&bars, Some(ny("2026-11-13T16:00:00"))), 156);
    }
    #[test]
    fn session_cut_half_day_and_dst_boundaries() {
        for (d, close) in [
            ("2026-03-06", "16:00:00"),
            ("2026-03-09", "16:00:00"),
            ("2026-10-30", "16:00:00"),
            ("2026-11-02", "16:00:00"),
            ("2026-11-27", "13:00:00"),
        ] {
            let bars = intrabars(&[d]);
            let ts = ny(&format!("{d}T{close}"));
            assert_eq!(session_cut(&bars, Some(ts - 1)), 0);
            assert_eq!(session_cut(&bars, Some(ts)), bars.len());
            assert_eq!(completed_session(ts).to_string(), d);
            assert_ne!(completed_session(ts - 1).to_string(), d);
        }
    }
    #[test]
    fn session_cut_without_observation_clock_drops_the_last_date() {
        let bars = intrabars(&["2026-11-12", "2026-11-13"]);
        assert_eq!(session_cut(&bars, None), 78);
    }
    #[test]
    fn gap_trims_to_contiguous_tail_and_short_tail_is_refused() {
        let bars = fixture(700);
        let info = StitchInfo { gaps: vec![(bars[299].ts, bars[300].ts)], ..Default::default() };
        let p = prepare(&bars, Timeframe::D1, AssetClass::Equity, Some(bars[699].ts + 86400), Some(&info)).unwrap();
        assert!(p.trimmed_gap);
        assert_eq!(p.bars[0].ts, bars[300].ts);
        let info = StitchInfo { gaps: vec![(bars[599].ts, bars[600].ts)], ..info };
        let miss =
            prepare(&bars, Timeframe::D1, AssetClass::Equity, Some(bars[699].ts + 86400), Some(&info)).err().unwrap();
        assert_eq!(miss.code, "SHORT HISTORY");
        assert!(miss.is_test);
        let info = StitchInfo { note: Some("QUARANTINED".into()), ..info };
        assert_eq!(
            prepare(&bars, Timeframe::D1, AssetClass::Equity, None, Some(&info)).err().unwrap().code,
            "QUARANTINED"
        );
    }
    #[test]
    fn all_same_regime_gives_zero_delta_p_one_and_never_passes() {
        let bars = fixture(1000);
        let proof = regime_edge(&bars, &vec![Some(1); 1000], &vec![false; 1000], &vec![false; 1000], &[5]).unwrap();
        let c = &proof.cells[0];
        assert_eq!(delta(c), Some(0.0));
        assert_eq!(c.p, Some(1.0));
        assert!(!passes(c, Some(1.0)));
    }
    #[test]
    fn summary_counts_quartiles_scale_and_small_wall_flag() {
        let mut edges =
            vec![display(-0.0002, 0.001), display(0.0, 0.5), display(0.0002, 0.001), DisplayEdge::pending()];
        let s = resolve(&mut edges);
        assert_eq!((s.m, s.resolved, s.pending), (4, 3, 1));
        assert_eq!((s.pass_up, s.pass_down), (1, 1));
        assert_eq!((s.median_bp, s.q1_bp, s.q3_bp), (Some(0.0), Some(-1.0), Some(1.0)));
        assert_eq!(s.scale_bp, 2.0);
        assert!(s.small);
        assert_eq!(s.lone_p, Some(0.025));
        assert_eq!(summary(&[]).scale_bp, 1.0);
        assert_eq!(summary(&[]).lone_p, None);
    }
    #[test]
    fn completed_session_before_and_after_close_weekend_and_holiday() {
        for (clock, expected) in [
            ("2026-11-13T15:59:59", "2026-11-12"),
            ("2026-11-13T16:00:00", "2026-11-13"),
            ("2026-11-14T12:00:00", "2026-11-13"),
            ("2026-11-26T17:00:00", "2026-11-25"),
        ] {
            assert_eq!(completed_session(ny(clock)).to_string(), expected);
        }
    }
}
