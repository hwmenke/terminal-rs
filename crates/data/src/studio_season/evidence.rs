//! One predeclared ALL + weekday clock family. Construction uses admitted bars only.
use super::*;
use crate::studio_inference::{HacEstimate, hac_mean, hac_mean_with_breaks};

#[derive(Debug, Clone, PartialEq)]
pub struct SeasonEvidenceCell {
    pub inference: HacEstimate,
    pub q: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeasonEvidence {
    /// Row major: ALL, Monday, ..., Friday. Every expected full-session bucket.
    pub cells: Vec<SeasonEvidenceCell>,
    pub buckets: usize,
    pub grid_sessions: usize,
    pub sessions: usize,
    pub expected_bars: usize,
    pub closed_thru: i64,
    /// Fractions; shared by all six rows, independent of presentation mode.
    pub heat_scale: f64,
    pub ribbon_scale: f64,
}

impl SeasonEvidence {
    pub fn cell(&self, row: usize, clock: usize) -> &SeasonEvidenceCell {
        &self.cells[row * self.buckets + clock]
    }
    pub fn family_size(&self) -> usize {
        self.cells.len()
    }
}

pub(super) fn build(
    tf: Timeframe,
    bars: &[Bar],
    dates: &[NaiveDate],
    returns: &[Option<f64>],
    gaps: &[(i64, i64)],
) -> SeasonEvidence {
    let mut grid = vec![dates[0]];
    while *grid.last().unwrap() < *dates.last().unwrap() {
        grid.push(next_session(*grid.last().unwrap()));
    }
    let step = (tf.seconds() / 60) as usize;
    let buckets = 390_usize.div_ceil(step);
    let mut values = vec![vec![None; grid.len()]; buckets];
    for ((b, d), r) in bars.iter().zip(dates).zip(returns) {
        let s = grid.binary_search(d).unwrap();
        let wall = tr_core::ny::civil(b.ts);
        let k = (wall.hour() * 60 + wall.minute() - 570) as usize / step;
        values[k][s] = *r;
    }
    let mut cells = Vec::with_capacity(6 * buckets);
    // Each clock keeps its own join boundaries: a join can occur within a session.
    let breaks: Vec<Vec<bool>> = (0..buckets)
        .map(|k| {
            if gaps.is_empty() {
                return Vec::new();
            }
            let minute = 570 + (k * step) as u32;
            let timestamps: Vec<_> = grid
                .iter()
                .map(|d| tr_core::ny::to_utc(d.and_hms_opt(minute / 60, minute % 60, 0).unwrap()))
                .collect();
            (0..grid.len())
                // Cut at the first clock at/after the right witness. A join may
                // span several missing sessions, so no adjacent grid pair need
                // contain both witnesses of the gap.
                .map(|s| s > 0 && gaps.iter().any(|&(_, after)| timestamps[s - 1] < after && timestamps[s] >= after))
                .collect()
        })
        .collect();
    for row in 0..6 {
        for k in 0..buckets {
            let selected: Vec<_> = values[k]
                .iter()
                .zip(&grid)
                .map(|(v, d)| {
                    if row == 0 || d.weekday().num_days_from_monday() as usize + 1 == row {
                        *v
                    } else {
                        None
                    }
                })
                .collect();
            let inference = if gaps.is_empty() {
                hac_mean(&selected)
            } else {
                hac_mean_with_breaks(&selected, &breaks[k])
            };
            cells.push(SeasonEvidenceCell { inference, q: 1. });
        }
    }
    let ps: Vec<_> = cells.iter().map(|c| c.inference.p.unwrap_or(1.)).collect();
    for (c, q) in cells.iter_mut().zip(crate::nulls::benjamini_yekutieli(&ps)) {
        c.q = q;
    }
    let heat_scale = cells.iter().filter_map(|c| c.inference.estimate).map(f64::abs).fold(0.0001, f64::max);
    let ribbon_scale = cells[..buckets]
        .iter()
        .map(|c| {
            let (lo, hi) = c.inference.interval.unwrap_or((0., 0.));
            lo.abs().max(hi.abs()).max(c.inference.estimate.unwrap_or(0.).abs())
        })
        .fold(0.0001, f64::max)
        * 1.1;
    let last = bars.last().unwrap();
    let close = close_minute(*dates.last().unwrap());
    let end = tr_core::ny::to_utc(dates.last().unwrap().and_hms_opt(close / 60, close % 60, 0).unwrap());
    SeasonEvidence {
        cells,
        buckets,
        grid_sessions: grid.len(),
        sessions: 1 + dates.windows(2).filter(|d| d[0] != d[1]).count(),
        expected_bars: grid.iter().map(|d| regular_opens(*d, tf).len()).sum(),
        closed_thru: (last.ts + tf.seconds()).min(end),
        heat_scale,
        ribbon_scale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(sessions: usize) -> Vec<Bar> {
        let mut d: NaiveDate = "2026-01-05".parse().unwrap();
        let mut bars = Vec::new();
        for s in 0..sessions {
            for (k,ts) in regular_opens(d,Timeframe::M5).into_iter().enumerate() {
                let c = 100. * (1. + ((s%7) as f64 - 2.) * 0.0001 + k as f64*0.000001);
                bars.push(Bar::new(ts,100.,101.,99.,c,10.).with_provenance(Provenance::Real));
            }
            d=next_session(d);
        }
        bars
    }
    fn report(b: &[Bar], gaps: &[(i64,i64)]) -> SeasonReport {
        let now=b.last().unwrap().ts+3600;
        compute_with_gaps("AAPL",Timeframe::M5,b,"test",now,now,gaps).unwrap()
    }
    fn direct(grid: &[Option<f64>], breaks: &[bool]) -> (f64,f64) {
        let n=grid.iter().flatten().count() as f64;
        let mean=grid.iter().flatten().sum::<f64>()/n;
        let scores: Vec<_>=grid.iter().map(|v|v.map(|r|(r-mean)/n).unwrap_or(0.)).collect();
        let mut v=0.;
        for s in 0..scores.len() {
            v+=scores[s]*scores[s];
            for l in 1..=5.min(s) {
                if !breaks[s-l+1..=s].iter().any(|b|*b) {
                    v+=2.*(1.-l as f64/6.)*scores[s]*scores[s-l];
                }
            }
        }
        (mean,v.sqrt())
    }
    #[test]
    fn known_mean_se_and_single_all_weekday_family() {
        let b=fixture(160);
        let r=report(&b,&[]); let e=r.evidence.as_ref().unwrap();
        assert_eq!((e.family_size(),e.buckets,e.grid_sessions),(468,78,160));
        let grid: Vec<_>=b.iter().step_by(78).map(|b|Some(b.close/b.open-1.)).collect();
        let (mean,se)=direct(&grid,&vec![false;160]);
        let i=&e.cell(0,0).inference;
        assert!((i.estimate.unwrap()-mean).abs()<1e-14);
        assert!((i.se.unwrap()-se).abs()<1e-14);
        assert!((i.interval.unwrap().0-(mean-1.96*se)).abs()<1e-14);
        for (k,c) in r.clock.iter().enumerate() {
            assert_eq!(e.cell(0,k).inference.n,c.returns.stats.count);
            assert!((e.cell(0,k).inference.estimate.unwrap()-c.returns.stats.mean.unwrap()).abs()<1e-14);
            for d in 0..5 { assert_eq!(e.cell(d+1,k).inference.n,c.weekdays[d].stats.count); }
        }
        let ps:Vec<_>=e.cells.iter().map(|c|c.inference.p.unwrap_or(1.)).collect();
        assert_eq!(e.cells.iter().map(|c|c.q).collect::<Vec<_>>(),crate::nulls::benjamini_yekutieli(&ps));
        assert!(e.cells[78..].iter().any(|c|c.inference.p.is_some()));
        // A row-only adjustment would give different q-values: ALL shares the family.
        assert_ne!(e.cells[..78].iter().map(|c|c.q).collect::<Vec<_>>(),crate::nulls::benjamini_yekutieli(&ps[..78]));
    }
    #[test]
    fn missing_sessions_preserve_distance_and_vault_joins_break_products() {
        let mut b=fixture(60);
        let all_grid:Vec<_>=b.iter().step_by(78).map(|b|Some(b.close/b.open-1.)).collect();
        let lost=tr_core::ny::civil(b[20*78].ts).date();
        b.retain(|b|tr_core::ny::civil(b.ts).date()!=lost);
        let gap=(b[29*78+2].ts,b[29*78+3].ts);
        let r=report(&b,&[gap]); let e=r.evidence.unwrap();
        assert_eq!((e.grid_sessions,e.sessions),(60,59));
        let mut grid=all_grid; grid[20]=None;
        let mut breaks=vec![false;60]; breaks[31]=true; // opening clock crosses intraday join on the following session
        let (_,se)=direct(&grid,&breaks);
        assert!((e.cell(0,0).inference.se.unwrap()-se).abs()<1e-14);
        let without=report(&b,&[]).evidence.unwrap();
        let (_,unbroken)=direct(&grid,&vec![false;60]);
        assert!((without.cell(0,0).inference.se.unwrap()-unbroken).abs()<1e-14);
        assert_ne!(without.cell(0,0).inference.variance,e.cell(0,0).inference.variance);
        let mut wide = fixture(60);
        let gap = (wide[29 * 78 + 77].ts, wide[32 * 78].ts);
        let grid: Vec<_> = wide.iter().step_by(78).enumerate().map(|(s,b)| {
            if s == 30 || s == 31 { None } else { Some(b.close / b.open - 1.) }
        }).collect();
        wide.retain(|b| !(b.ts > gap.0 && b.ts < gap.1));
        let mut boundaries = vec![false; 60]; boundaries[32] = true;
        let (_, expected) = direct(&grid, &boundaries);
        let e = report(&wide, &[gap]).evidence.unwrap();
        assert_eq!((e.grid_sessions, e.sessions), (60, 58));
        assert!((e.cell(0,0).inference.se.unwrap() - expected).abs() < 1e-14);
    }
    #[test]
    fn half_day_sparse_zero_and_source_closure() {
        let mut b: Vec<_>=["2026-11-27","2026-11-30"].iter().flat_map(|d|regular_opens(d.parse().unwrap(),Timeframe::M5)).map(|ts|
            Bar::new(ts,100.,100.,100.,100.,0.).with_provenance(Provenance::Real)).collect();
        let r=report(&b,&[]); let e=r.evidence.unwrap();
        assert_eq!((e.expected_bars,e.cell(0,0).inference.n,e.cell(0,42).inference.n),(120,2,1));
        assert!(e.cells.iter().all(|c|c.inference.p.is_none() && c.q==1.));
        let observed=b.last().unwrap().ts+299;
        let r=compute_at("AAPL",Timeframe::M5,&b,"test",observed,observed+86400).unwrap();
        assert_eq!(r.evidence.unwrap().closed_thru,b[b.len()-2].ts+300);
        b.retain(|b|tr_core::ny::civil(b.ts).date().to_string()=="2026-11-27");
        let e=report(&b,&[]).evidence.unwrap();
        assert!(e.cell(0,42).inference.estimate.is_none());
        assert_eq!(e.cell(0,0).inference.estimate,Some(0.));
        let mut b=fixture(35); for b in &mut b { b.close=b.open; }
        assert!(report(&b,&[]).evidence.unwrap().cells.iter().all(|c|c.inference.p.is_none() && c.q==1.));
    }
    #[test]
    fn full_input_refusal_before_pending_filter_and_no_daily_inference() {
        let b=fixture(35); let observed=b[100].ts;
        for p in [Provenance::Sample,Provenance::Missing,Provenance::Cached] {
            let mut bad=b.clone(); bad.last_mut().unwrap().provenance=p;
            assert!(compute_at("AAPL",Timeframe::M5,&bad,"test",observed,observed).is_err());
        }
        let mut bad=b.clone(); bad.last_mut().unwrap().close=f64::NAN;
        assert!(compute_at("AAPL",Timeframe::M5,&bad,"test",observed,observed).is_err());
        let mut bad=b.clone(); bad.last_mut().unwrap().volume=-1.;
        assert!(compute_at("AAPL",Timeframe::M5,&bad,"test",observed,observed).is_err());
        let mut bad=b.clone(); bad.last_mut().unwrap().ts+=1;
        assert!(compute_at("AAPL",Timeframe::M5,&bad,"test",observed,observed).is_err());
        let daily:Vec<_>=b.iter().step_by(78).map(|b| {
            let d=tr_core::ny::civil(b.ts).date(); Bar { ts:d.and_hms_opt(0,0,0).unwrap().and_utc().timestamp(),..*b }
        }).collect();
        assert!(compute("AAPL",Timeframe::D1,&daily,"test",b.last().unwrap().ts+86400).unwrap().evidence.is_none());
    }
}
