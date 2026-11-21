//! Price-free provenance and gap metadata. This module never creates a Bar.
use crate::intraday::{self, Freshness, MarketClock};
use tr_core::{AssetClass, Bar, Provenance, Timeframe};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailKind {
    Real,
    Cached,
    Hatched,
    Stale { lag_secs: i64 },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RailCell {
    /// Actual bar index, or bars.len() for the stale tail.
    pub bar: usize,
    pub ts: i64,
    pub until: i64,
    pub kind: RailKind,
    /// Missing opens immediately before this actual bar. No price is assigned.
    pub holes: Vec<i64>,
}

pub fn rail_cells(tf: Timeframe, bars: &[Bar], class: AssetClass, now: i64) -> Vec<RailCell> {
    if !tf.is_intraday() {
        return Vec::new();
    }
    let mut cells: Vec<_> = bars
        .iter()
        .enumerate()
        .map(|(bar, b)| RailCell {
            bar,
            ts: b.ts,
            until: b.ts,
            kind: match b.provenance {
                Provenance::Real => RailKind::Real,
                Provenance::Cached => RailKind::Cached,
                _ => RailKind::Hatched,
            },
            holes: Vec::new(),
        })
        .collect();
    let clock = MarketClock::for_class(class);
    let holes = if clock == MarketClock::UsCash {
        intraday::session_holes(tf, bars)
    } else {
        bars.windows(2)
            .flat_map(|w| {
                let first = w[0].ts.saturating_add(tf.seconds());
                (first..w[1].ts).step_by(tf.seconds() as usize)
            })
            .collect()
    };
    for ts in holes {
        let next = bars.partition_point(|b| b.ts < ts);
        if let Some(cell) = cells.get_mut(next) {
            cell.holes.push(ts);
        }
    }
    if let Some(last) = bars.last()
        && let Some(Freshness::Stale { lag_secs }) = intraday::freshness(clock, tf, last.ts, now)
    {
        cells.push(RailCell {
            bar: bars.len(),
            ts: last.ts,
            until: now,
            kind: RailKind::Stale { lag_secs },
            holes: Vec::new(),
        });
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Vec<Bar> {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        let open = tr_core::ny::to_utc(date.and_hms_opt(9, 30, 0).unwrap());
        (0..12)
            .map(|i| Bar::new(open + i * 300, 100.0, 101.0, 99.0, 100.0, 1.0).with_provenance(Provenance::Real))
            .collect()
    }
    #[test]
    fn rail_marks_every_missing_bucket_and_never_fills_it() {
        let original = fixture();
        let bars: Vec<_> =
            original.iter().enumerate().filter(|(i, _)| ![3, 4, 8].contains(i)).map(|(_, b)| *b).collect();
        let before = bars.clone();
        let cells = rail_cells(Timeframe::M5, &bars, AssetClass::Equity, bars.last().unwrap().ts);
        assert_eq!(cells.len(), bars.len());
        assert_eq!(
            cells.iter().flat_map(|c| c.holes.iter()).copied().collect::<Vec<_>>(),
            [3, 4, 8].map(|i| original[i].ts)
        );
        assert_eq!(bars, before);
        assert!(cells.iter().all(|c| bars[c.bar].ts == c.ts));
    }
    #[test]
    fn rail_tail_is_stale_only_past_freshness_window() {
        let bars = fixture();
        let last = bars.last().unwrap().ts;
        for (age, stale) in [(600, false), (601, true)] {
            let cells = rail_cells(Timeframe::M5, &bars, AssetClass::Crypto, last + age);
            assert_eq!(cells.iter().any(|c| matches!(c.kind, RailKind::Stale { .. })), stale);
            assert_eq!(cells.len(), bars.len() + usize::from(stale));
        }
        let date = tr_core::ny::civil(last).date();
        let closed = tr_core::ny::to_utc(date.and_hms_opt(17, 0, 0).unwrap());
        let mut close = bars.clone();
        close.last_mut().unwrap().ts = tr_core::ny::to_utc(date.and_hms_opt(15, 55, 0).unwrap());
        assert!(
            !rail_cells(Timeframe::M5, &close, AssetClass::Equity, closed)
                .iter()
                .any(|c| matches!(c.kind, RailKind::Stale { .. }))
        );
    }
    #[test]
    fn rail_sample_bars_are_hatched() {
        let mut bars = fixture();
        bars[2].provenance = Provenance::Sample;
        bars[5].provenance = Provenance::Missing;
        bars[6].provenance = Provenance::Cached;
        let cells = rail_cells(Timeframe::M5, &bars, AssetClass::Equity, bars.last().unwrap().ts);
        assert_eq!(cells[2].kind, RailKind::Hatched);
        assert_eq!(cells[5].kind, RailKind::Hatched);
        assert_eq!(cells[6].kind, RailKind::Cached);
        assert_eq!(cells[0].kind, RailKind::Real);
    }
}
