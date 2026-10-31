use serde::{Deserialize, Serialize};

use crate::{Provenance, Timeframe};

/// One OHLCV bar. `ts` is the bar open time in Unix seconds (UTC).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    #[serde(default)]
    pub provenance: Provenance,
    pub ts: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Missing volume is NaN, distinct from a reported zero. JSON stores it as null.
    #[serde(default = "missing_volume", deserialize_with = "deserialize_volume")]
    pub volume: f64,
}

fn missing_volume() -> f64 {
    f64::NAN
}

fn deserialize_volume<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::NAN))
}

impl Bar {
    pub fn new(ts: i64, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Self {
        Self { ts, open, high, low, close, volume, provenance: Provenance::Missing }
    }

    /// Validate prices at provider ingestion; missing volume does not invalidate prices.
    pub fn valid_ohlc(&self) -> bool {
        [self.open, self.high, self.low, self.close].iter().all(|p| p.is_finite() && *p > 0.0)
            && self.high >= self.open.max(self.close)
            && self.low <= self.open.min(self.close)
            && self.high >= self.low
    }

    /// A reported, finite, nonnegative volume (zero is a valid observation).
    pub fn volume_value(&self) -> Option<f64> {
        (self.volume.is_finite() && self.volume >= 0.0).then_some(self.volume)
    }

    pub fn with_provenance(mut self, provenance: Provenance) -> Self {
        self.provenance = provenance;
        self
    }

    pub fn provenance(bars: &[Self]) -> Provenance {
        bars.iter().map(|b| b.provenance).reduce(Provenance::combine).unwrap_or_default()
    }

    /// Shared gate for scans, research and historical performance/backtest callers.
    pub fn require_verified(bars: &[Self], operation: &str) -> Result<(), String> {
        Self::provenance(bars).require_verified(operation)
    }

    /// Typical price (high + low + close) / 3, used by VWAP.
    pub fn hlc3(&self) -> f64 {
        (self.high + self.low + self.close) / 3.0
    }

    pub fn is_up(&self) -> bool {
        self.close >= self.open
    }

    /// Merge a live update for the same bar: keep the original open, widen the range,
    /// take the latest close and the larger volume (partial streams undercount).
    pub fn absorb(&mut self, live: &Bar) {
        debug_assert_eq!(self.ts, live.ts);
        self.high = self.high.max(live.high);
        self.low = self.low.min(live.low);
        self.close = live.close;
        self.volume = self.volume.max(live.volume);
        self.provenance = self.provenance.combine(live.provenance);
    }
}

/// A symbol's bars at one timeframe, sorted by `ts` ascending with no duplicates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Series {
    pub symbol: String,
    pub timeframe: Timeframe,
    pub bars: Vec<Bar>,
}

impl Series {
    pub fn provenance(&self) -> Provenance {
        Bar::provenance(&self.bars)
    }

    pub fn new(symbol: impl Into<String>, timeframe: Timeframe, bars: Vec<Bar>) -> Self {
        let mut s = Self { symbol: symbol.into(), timeframe, bars };
        s.normalize();
        s
    }

    /// Sort by time and drop duplicate timestamps (last write wins).
    pub fn normalize(&mut self) {
        self.bars.sort_by_key(|b| b.ts);
        let mut out: Vec<Bar> = Vec::with_capacity(self.bars.len());
        for b in self.bars.drain(..) {
            match out.last_mut() {
                Some(last) if last.ts == b.ts => *last = b,
                _ => out.push(b),
            }
        }
        self.bars = out;
    }

    pub fn len(&self) -> usize {
        self.bars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    pub fn closes(&self) -> Vec<f64> {
        self.bars.iter().map(|b| b.close).collect()
    }

    /// Index of the first bar with `ts >= t` (may equal `len()`).
    pub fn lower_bound(&self, t: i64) -> usize {
        self.bars.partition_point(|b| b.ts < t)
    }

    /// Index of the bar containing time `t`, i.e. the last bar with `ts <= t`.
    pub fn index_at_or_before(&self, t: i64) -> Option<usize> {
        let i = self.bars.partition_point(|b| b.ts <= t);
        i.checked_sub(1)
    }

    /// Apply one live bar: absorb it into the last bar if it is the same period, append it
    /// if newer, ignore it if older. Returns true if the series changed.
    pub fn apply_live(bars: &mut Vec<Bar>, live: Bar) -> bool {
        match bars.last_mut() {
            Some(last) if last.ts == live.ts => {
                let before = *last;
                last.absorb(&live);
                *last != before
            }
            Some(last) if last.ts > live.ts => false,
            _ => {
                bars.push(live);
                true
            }
        }
    }

    /// Merge newer bars (e.g. a streaming update) into the series.
    pub fn merge(&mut self, bars: impl IntoIterator<Item = Bar>) {
        let mut tail_only = true;
        for b in bars {
            match self.bars.last_mut() {
                Some(last) if last.ts == b.ts => *last = b,
                Some(last) if last.ts > b.ts => {
                    tail_only = false;
                    self.bars.push(b);
                }
                _ => self.bars.push(b),
            }
        }
        if !tail_only {
            self.normalize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(ts: i64, c: f64) -> Bar {
        Bar::new(ts, c, c, c, c, 1.0)
    }

    #[test]
    fn validates_ohlc_and_preserves_missing_volume() {
        let valid = Bar::new(0, 10.0, 12.0, 9.0, 11.0, f64::NAN);
        assert!(valid.valid_ohlc());
        assert_eq!(valid.volume_value(), None);
        for bad in [
            Bar { high: 10.5, ..valid },
            Bar { low: 10.5, ..valid },
            Bar { high: 8.0, low: 9.0, ..valid },
            Bar { open: 0.0, ..valid },
            Bar { low: -1.0, ..valid },
            Bar { close: f64::NAN, ..valid },
            Bar { high: f64::INFINITY, ..valid },
            Bar { open: f64::NEG_INFINITY, ..valid },
        ] {
            assert!(!bad.valid_ohlc(), "{bad:?}");
        }
        let json = serde_json::to_string(&valid).unwrap();
        let restored: Bar = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.volume_value(), None);
        assert_eq!(Bar { volume: 0.0, ..valid }.volume_value(), Some(0.0));
        let resampled = Timeframe::D1.resample(&[valid, Bar { ts: 60, volume: 1.0, ..valid }]);
        assert_eq!(resampled[0].volume_value(), Some(1.0));
    }

    #[test]
    fn missing_metadata_and_mixed_prices_are_never_verified() {
        let legacy: Bar = serde_json::from_str(r#"{"ts":0,"open":1,"high":1,"low":1,"close":1,"volume":1}"#).unwrap();
        assert_eq!(legacy.provenance, Provenance::Missing);
        let real = b(0, 1.0).with_provenance(Provenance::Real);
        let sample = b(1, 2.0).with_provenance(Provenance::Sample);
        let bars = vec![real, sample];
        assert_eq!(Series::new("X", Timeframe::M1, bars.clone()).provenance(), Provenance::Sample);
        assert_eq!(Timeframe::D1.resample(&bars)[0].provenance, Provenance::Sample);
        let mut absorbed = real;
        absorbed.absorb(&Bar { ts: 0, ..sample });
        assert_eq!(absorbed.provenance, Provenance::Sample);
        for operation in ["Research", "Scan", "Backtest"] {
            assert!(Bar::require_verified(&bars, operation).unwrap_err().contains("SAMPLE DATA"));
            assert!(Bar::require_verified(&[legacy], operation).unwrap_err().contains("UNVERIFIED"));
            assert!(Bar::require_verified(&[], operation).is_err());
            assert!(Bar::require_verified(&[real], operation).is_ok());
        }
    }

    #[test]
    fn normalize_sorts_and_dedups() {
        let s = Series::new("X", Timeframe::D1, vec![b(3, 3.0), b(1, 1.0), b(3, 30.0), b(2, 2.0)]);
        let ts: Vec<_> = s.bars.iter().map(|b| (b.ts, b.close)).collect();
        assert_eq!(ts, vec![(1, 1.0), (2, 2.0), (3, 30.0)]);
    }

    #[test]
    fn live_updates() {
        let mut v = vec![Bar::new(60, 10.0, 11.0, 9.0, 10.5, 100.0)];
        assert!(Series::apply_live(&mut v, Bar::new(60, 10.4, 12.0, 10.0, 11.5, 40.0)));
        assert_eq!(v[0], Bar::new(60, 10.0, 12.0, 9.0, 11.5, 100.0));
        assert!(!Series::apply_live(&mut v, Bar::new(0, 1.0, 1.0, 1.0, 1.0, 1.0)));
        assert!(Series::apply_live(&mut v, Bar::new(120, 11.5, 11.6, 11.4, 11.6, 5.0)));
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn lookup_and_merge() {
        let mut s = Series::new("X", Timeframe::D1, vec![b(10, 1.0), b(20, 2.0), b(30, 3.0)]);
        assert_eq!(s.index_at_or_before(25), Some(1));
        assert_eq!(s.index_at_or_before(5), None);
        assert_eq!(s.lower_bound(20), 1);
        s.merge([b(30, 3.5), b(40, 4.0)]);
        assert_eq!(s.len(), 4);
        assert_eq!(s.bars[2].close, 3.5);
        s.merge([b(15, 1.5)]);
        assert_eq!(s.bars[1].ts, 15);
    }
}
