//! Native 30m US cash opening-range conditions and completed forward outcomes.
use crate::{
    history::calendar::next_session,
    intraday::{regular_bucket, regular_opens},
    studio_dist::spans_gap,
    studio_inference::{self, HacEstimate},
};
use chrono::{DateTime, NaiveDate, Utc};
use std::collections::BTreeMap;
use tr_core::{Bar, Provenance, Timeframe};

pub const OR_FAMILY: usize = 3 * 13 * 3;
pub const OR_HORIZONS: [usize; 3] = [1, 2, 4];
pub const OR_BINS: usize = 20;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrState {
    Above,
    Below,
    Inside,
}
impl OrState {
    pub const ALL: [Self; 3] = [Self::Above, Self::Below, Self::Inside];
    pub fn label(self) -> &'static str {
        match self {
            Self::Above => "Above",
            Self::Below => "Below",
            Self::Inside => "Inside",
        }
    }
    pub fn classify(close: f64, opening: &Bar) -> Self {
        if close > opening.high {
            Self::Above
        } else if close < opening.low {
            Self::Below
        } else {
            Self::Inside
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct OrHistogram {
    pub edges: Vec<f64>,
    pub counts: [Vec<usize>; 2],
    pub mass: [Vec<f64>; 2],
    pub max_mass: f64,
}
impl OrHistogram {
    fn new(a: &[f64], b: &[f64]) -> Self {
        let lo = a.iter().chain(b).copied().fold(f64::INFINITY, f64::min);
        let hi = a.iter().chain(b).copied().fold(f64::NEG_INFINITY, f64::max);
        let (lo, hi) = if !lo.is_finite() {
            (-1., 1.)
        } else if hi - lo < 1e-9 {
            (lo - 0.5, hi + 0.5)
        } else {
            (lo, hi)
        };
        let step = (hi - lo) / OR_BINS as f64;
        let edges = (0..=OR_BINS).map(|i| lo + i as f64 * step).collect();
        let counts = [a, b].map(|v| {
            let mut c = vec![0; OR_BINS];
            for x in v {
                c[(((x - lo) / step).floor() as usize).min(OR_BINS - 1)] += 1;
            }
            c
        });
        let mass: [Vec<f64>; 2] = [0, 1].map(|i| {
            let n = if i == 0 { a.len() } else { b.len() };
            counts[i].iter().map(|c| if n > 0 { *c as f64 / n as f64 } else { 0. }).collect()
        });
        let max_mass = mass.iter().flatten().copied().fold(0.05, f64::max);
        Self { edges, counts, mass, max_mass }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct OrCell {
    pub state: OrState,
    /// Origin CLOSE minute in New York; 600 = 10:00.
    pub minute: u32,
    pub horizon: usize,
    pub selected: Vec<f64>,
    pub complement: Vec<f64>,
    pub mean: Option<f64>,
    pub complement_mean: Option<f64>,
    pub all_mean: Option<f64>,
    pub inference: HacEstimate,
    pub q: f64,
    pub whisker_scale: f64,
    pub histogram: OrHistogram,
}
#[derive(Clone, Debug, PartialEq)]
pub struct OrLens {
    pub symbol: String,
    pub source: String,
    pub provenance: Provenance,
    pub observed_at: i64,
    pub closed_thru: i64,
    pub start: String,
    pub end: String,
    pub bars: usize,
    pub expected_bars: usize,
    pub sessions: usize,
    pub opening_sessions: usize,
    pub grid_sessions: usize,
    pub cells: Vec<OrCell>,
    pub effect_scale: f64,
    pub gaps: Vec<(i64, i64)>,
}
impl OrLens {
    pub fn cell(&self, state: usize, clock: usize, horizon: usize) -> &OrCell {
        &self.cells[(state * 13 + clock) * 3 + horizon]
    }
}
fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Complete input validation precedes filtering. Production callers must preserve
/// the existing analysis_bars/vault::research_bars witness boundary.
pub fn opening_range_lens(
    symbol: &str,
    tf: Timeframe,
    input: &[Bar],
    source: &str,
    observed: i64,
    now: i64,
    gaps: &[(i64, i64)],
) -> Result<OrLens, String> {
    if tf != Timeframe::M30 {
        return Err("OR Lens requires native 30m US cash bars".into());
    }
    if !crate::cash_session::is_cash(&crate::security(symbol)) {
        return Err("OR Lens requires a US cash-session security".into());
    }
    crate::stats::require_research_bars(input, "OR Lens")?;
    crate::vault::research_bars(input, None)?;
    DateTime::<Utc>::from_timestamp(observed, 0).ok_or("OR Lens: invalid observation clock")?;
    DateTime::<Utc>::from_timestamp(now, 0).ok_or("OR Lens: invalid calendar clock")?;
    let today = tr_core::ny::civil(now).date();
    let mut sessions: BTreeMap<NaiveDate, BTreeMap<i64, Bar>> = BTreeMap::new();
    let mut closed_thru = 0;
    let mut count = 0;
    for b in input {
        DateTime::<Utc>::from_timestamp(b.ts, 0).ok_or("OR Lens: invalid timestamp")?;
        if b.volume < 0. || b.high < b.open.max(b.close) || b.low > b.open.min(b.close) || b.low > b.high {
            return Err("OR Lens refused: invalid OHLCV".into());
        }
        if !crate::intraday::in_regular_session(b.ts) {
            continue;
        }
        if regular_bucket(tf, b.ts) != Some(b.ts) {
            return Err("OR Lens refused: timestamps must match native 30m session grid".into());
        }
        let d = tr_core::ny::civil(b.ts).date();
        if d >= today || b.ts + tf.seconds() > observed.min(now) {
            continue;
        }
        sessions.entry(d).or_default().insert(b.ts, *b);
        count += 1;
        closed_thru = closed_thru.max(b.ts + tf.seconds());
    }
    let first =
        *sessions.first_key_value().ok_or("OR Lens unavailable: no completed historical sessions (TODAY excluded)")?.0;
    let last = *sessions.last_key_value().unwrap().0;
    let mut dates = vec![first];
    while *dates.last().unwrap() < last {
        dates.push(next_session(*dates.last().unwrap()));
    }
    // One observation for each expected session × origin close × horizon.
    // Missing sessions stay None, preserving exchange-session lag distance.
    let mut outcomes = vec![vec![None; dates.len()]; 13 * 3];
    let mut expected_bars = 0;
    let mut opening_sessions = 0;
    for (s, d) in dates.iter().enumerate() {
        let opens = regular_opens(*d, tf);
        expected_bars += opens.len();
        let Some(bars) = sessions.get(d) else { continue };
        let Some(opening) = bars.get(&opens[0]) else { continue };
        opening_sessions += 1;
        for (k, ts) in opens.iter().enumerate() {
            let Some(origin) = bars.get(ts) else { continue };
            if spans_gap(gaps, opening.ts, origin.ts) {
                continue;
            }
            let state = OrState::classify(origin.close, opening);
            for (hi, h) in OR_HORIZONS.iter().enumerate() {
                let Some(end_ts) = opens.get(k + h) else { continue };
                if spans_gap(gaps, *ts, *end_ts) || !opens[k..=k + h].iter().all(|t| bars.contains_key(t)) {
                    continue;
                }
                let r = 1e4 * (bars[end_ts].close / origin.close - 1.);
                if r.is_finite() {
                    outcomes[k * 3 + hi][s] = Some((state, r));
                }
            }
        }
    }
    let mut cells = Vec::with_capacity(OR_FAMILY);
    for state in OrState::ALL {
        for k in 0..13 {
            for (hi, horizon) in OR_HORIZONS.iter().enumerate() {
                let grid: Vec<_> = outcomes[k * 3 + hi].iter().map(|o| o.map(|(g, r)| (g == state, r))).collect();
                let selected: Vec<_> = grid.iter().flatten().filter(|(g, _)| *g).map(|(_, r)| *r).collect();
                let complement: Vec<_> = grid.iter().flatten().filter(|(g, _)| !*g).map(|(_, r)| *r).collect();
                let total_n = selected.len() + complement.len();
                let all_mean = (total_n > 0).then(|| selected.iter().chain(&complement).sum::<f64>() / total_n as f64);
                cells.push(OrCell {
                    state,
                    minute: 600 + k as u32 * 30,
                    horizon: *horizon,
                    mean: mean(&selected),
                    complement_mean: mean(&complement),
                    all_mean,
                    inference: studio_inference::hac_contrast(&grid),
                    q: 1.,
                    whisker_scale: 1.,
                    histogram: OrHistogram::new(&selected, &complement),
                    selected,
                    complement,
                });
            }
        }
    }
    let ps: Vec<_> = cells.iter().map(|c| c.inference.p.unwrap_or(1.)).collect();
    for (c, q) in cells.iter_mut().zip(crate::nulls::benjamini_yekutieli(&ps)) {
        c.q = q;
        let (lo, hi) = c.inference.interval.unwrap_or((0., 0.));
        c.whisker_scale = lo.abs().max(hi.abs()).max(c.inference.estimate.unwrap_or(0.).abs()).max(1.) * 1.1;
    }
    let effect_scale = cells.iter().filter_map(|c| c.inference.estimate).map(f64::abs).fold(1., f64::max);
    Ok(OrLens {
        symbol: symbol.into(),
        source: source.into(),
        provenance: Bar::provenance(input),
        observed_at: observed,
        closed_thru,
        start: first.to_string(),
        end: last.to_string(),
        bars: count,
        expected_bars,
        sessions: sessions.len(),
        opening_sessions,
        grid_sessions: dates.len(),
        cells,
        effect_scale,
        gaps: gaps.iter().copied().filter(|g| g.1 >= regular_opens(first, tf)[0] && g.0 < closed_thru).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bars(dates: &[&str]) -> Vec<Bar> {
        dates
            .iter()
            .enumerate()
            .flat_map(|(s, d)| {
                regular_opens(d.parse().unwrap(), Timeframe::M30).into_iter().enumerate().map(move |(k, ts)| {
                    let c = if k == 0 { 100. } else { [102., 98., 100.][s % 3] + k as f64 * 0.01 };
                    Bar::new(ts, c, if k == 0 { 101. } else { c + 0.1 }, if k == 0 { 99. } else { c - 0.1 }, c, 10.)
                        .with_provenance(Provenance::Real)
                })
            })
            .collect()
    }
    fn report(b: &[Bar], observed: i64, now: i64, gaps: &[(i64, i64)]) -> OrLens {
        opening_range_lens("AAPL", Timeframe::M30, b, "test", observed, now, gaps).unwrap()
    }
    fn clock(b: &[Bar]) -> i64 {
        b.last().unwrap().ts + 86400
    }
    #[test]
    fn hand_condition_return_contrast_and_same_clock_complement() {
        let b = bars(&["2026-03-06", "2026-03-09", "2026-03-10"]);
        let r = report(&b, clock(&b), clock(&b), &[]);
        assert_eq!(r.cells.len(), 117);
        assert_eq!(r.cell(0, 1, 0).minute, 630);
        let c = r.cell(0, 1, 0);
        assert_eq!(c.selected.len(), 1);
        assert_eq!(c.complement.len(), 2);
        let a = 1e4 * (102.02 / 102.01 - 1.);
        let other = [1e4 * (98.02 / 98.01 - 1.), 1e4 * (100.02 / 100.01 - 1.)];
        assert!((c.mean.unwrap() - a).abs() < 1e-10);
        assert!((c.inference.estimate.unwrap() - (a - (other[0] + other[1]) / 2.)).abs() < 1e-10);
        assert!((c.all_mean.unwrap() - (a + other[0] + other[1]) / 3.).abs() < 1e-10);
        assert_eq!(r.cell(2, 0, 0).selected.len(), 3); // 10:00 origin is always Inside.
        assert_eq!(r.cell(0, 0, 0).selected.len(), 0);
        let opening = b[0];
        assert_eq!(OrState::classify(101., &opening), OrState::Inside);
        assert_eq!(OrState::classify(99., &opening), OrState::Inside);
        assert_eq!(OrState::classify(101.01, &opening), OrState::Above);
        assert_eq!(OrState::classify(98.99, &opening), OrState::Below);
        assert!(r.cells.iter().all(|c| c.q == 1. && c.inference.p.is_none()));
        for i in 0..3 {
            assert!(r.cell(i, 12, 0).all_mean.is_none());
        }
    }
    #[test]
    fn future_prices_cannot_rewrite_earlier_condition() {
        let mut b = bars(&["2026-03-09"]);
        let before = OrState::classify(b[1].close, &b[0]);
        let now = clock(&b);
        let r = report(&b, now, now, &[]);
        for bar in &mut b[2..] {
            bar.open = 80.;
            bar.close = 80.;
            bar.high = 81.;
            bar.low = 79.;
        }
        let changed = report(&b, now, now, &[]);
        assert_eq!(OrState::classify(b[1].close, &b[0]), before);
        assert_eq!(r.cell(0, 1, 0).selected.len(), changed.cell(0, 1, 0).selected.len());
        assert_ne!(r.cell(0, 1, 0).mean, changed.cell(0, 1, 0).mean);
    }
    #[test]
    fn missing_opening_and_intermediate_buckets_exclude_exact_windows() {
        let b = bars(&["2026-03-09", "2026-03-10"]);
        let mut missing = b.clone();
        missing.remove(0);
        let r = report(&missing, clock(&b), clock(&b), &[]);
        assert_eq!(r.opening_sessions, 1);
        assert_eq!(r.cell(0, 1, 0).selected.len(), 0);
        assert_eq!(r.cell(1, 1, 0).selected.len(), 1);
        let mut missing = b.clone();
        missing.remove(2);
        let r = report(&missing, clock(&b), clock(&b), &[]);
        assert_eq!(r.cell(0, 1, 0).selected.len(), 0);
        assert_eq!(r.cell(0, 1, 1).selected.len(), 0); // endpoint exists, intervening absent
        assert_eq!(r.cell(0, 3, 0).selected.len(), 1); // later valid origin stays valid
    }
    #[test]
    fn half_day_forming_endpoint_today_and_vault_gap() {
        let b = bars(&["2026-11-27", "2026-11-30"]); // seven buckets on half day
        let r = report(&b, clock(&b), clock(&b), &[]);
        assert_eq!(r.expected_bars, 20);
        assert_eq!(r.cell(0, 5, 0).selected.len(), 1);
        assert_eq!(r.cell(0, 6, 0).selected.len(), 0); // 13:00 cannot substitute an overnight print
        assert_eq!(r.cell(0, 3, 2).selected.len(), 0);
        let r = report(&b, b[6].ts + 1799, clock(&b), &[]);
        assert_eq!(r.cell(0, 5, 0).selected.len(), 0); // 12:30->13:00 endpoint still forming
        assert_eq!(r.closed_thru, b[5].ts + 1800);
        let today = report(&b, clock(&b), b.last().unwrap().ts + 1800, &[]);
        assert_eq!(today.sessions, 1);
        assert_eq!(today.end, "2026-11-27");
        let gap = (b[2].ts, b[3].ts);
        let r = report(&b, clock(&b), clock(&b), &[gap]);
        assert_eq!(r.gaps, vec![gap]);
        assert_eq!(r.cell(0, 1, 0).selected.len(), 1);
        assert_eq!(r.cell(0, 1, 1).selected.len(), 0);
        assert_eq!(r.cell(0, 4, 0).selected.len(), 0); // condition cannot span unverified OR join
    }
    #[test]
    fn full_input_admission_precedes_historical_filtering() {
        let b = bars(&["2026-03-09", "2026-03-10"]);
        let now = b.last().unwrap().ts + 1800;
        for p in [Provenance::Sample, Provenance::Missing, Provenance::Cached] {
            let mut bad = b.clone();
            bad[20].provenance = p;
            assert!(opening_range_lens("AAPL", Timeframe::M30, &bad, "test", now, now, &[]).is_err());
        }
        let mut bad = b.clone();
        bad[20].close = f64::NAN;
        assert!(opening_range_lens("AAPL", Timeframe::M30, &bad, "test", now, now, &[]).is_err());
        let mut bad = b.clone();
        bad[20].ts += 1;
        assert!(opening_range_lens("AAPL", Timeframe::M30, &bad, "test", now, now, &[]).is_err());
        assert!(opening_range_lens("AAPL", Timeframe::M5, &b, "test", now, now, &[]).is_err());
        assert!(opening_range_lens("BTCUSD Curncy", Timeframe::M30, &b, "test", now, now, &[]).is_err());
    }
    #[test]
    fn session_grid_keeps_missing_sessions_and_shared_histograms_normalize() {
        let mut dates = Vec::new();
        let mut d: NaiveDate = "2026-01-05".parse().unwrap();
        for _ in 0..93 {
            dates.push(d.to_string());
            d = next_session(d);
        }
        let refs: Vec<_> = dates.iter().map(String::as_str).collect();
        let mut b = bars(&refs);
        let lost = refs[45].parse::<NaiveDate>().unwrap();
        b.retain(|b| tr_core::ny::civil(b.ts).date() != lost);
        let r = report(&b, clock(&b), clock(&b), &[]);
        assert_eq!((r.grid_sessions, r.sessions), (93, 92));
        let c = r.cell(0, 1, 0);
        assert_eq!(c.inference.span, 93);
        assert!(c.inference.n >= 30 && c.inference.complement_n >= 30 && c.inference.p.is_some());
        for c in &r.cells {
            for (g, v) in [&c.selected, &c.complement].iter().enumerate() {
                assert_eq!(c.histogram.counts[g].iter().sum::<usize>(), v.len());
                let mass = c.histogram.mass[g].iter().sum::<f64>();
                assert!((mass - if v.is_empty() { 0. } else { 1. }).abs() < 1e-12);
            }
            assert_eq!(c.histogram.edges.len(), OR_BINS + 1);
        }
        let ps: Vec<_> = r.cells.iter().map(|c| c.inference.p.unwrap_or(1.)).collect();
        assert_eq!(r.cells.iter().map(|c| c.q).collect::<Vec<_>>(), crate::nulls::benjamini_yekutieli(&ps));
    }
}
