//! Daily desk metrics. Missing history remains unavailable.
use super::{Ohlcv, atr, rsi, sma};
use crate::Bar;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stretch {
    pub pct: f64,
    pub atr_units: Option<f64>,
}

/// Signed distance from SMA50, in percent and Wilder ATR14 units.
pub fn sma50_stretch(bars: &[Bar]) -> Option<Stretch> {
    let o = Ohlcv::new(bars);
    let close = *o.close.last()?;
    let ma = *sma(&o.close, 50).last()?;
    if !close.is_finite() || close <= 0.0 || !ma.is_finite() || ma <= 0.0 {
        return None;
    }
    let a = *atr(&o, 14).last()?;
    let pct = (close / ma - 1.0) * 100.0;
    let units = (close - ma) / a;
    pct.is_finite().then_some(Stretch { pct, atr_units: (a > 0.0 && units.is_finite()).then_some(units) })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HoldingRisk {
    pub above_sma50: Option<bool>,
    pub rsi14: Option<f64>,
    pub concentrated: Option<bool>,
}

/// Count holdings with at least one known breach; missing values do not imply a breach.
pub fn risk_breaches(risks: impl IntoIterator<Item = HoldingRisk>) -> usize {
    risks.into_iter().filter(|r| r.above_sma50 == Some(false)
        || r.rsi14.is_some_and(|v| v.is_finite() && (v >= 70.0 || v <= 30.0))
        || r.concentrated == Some(true)).count()
}

/// Weight and threshold are fractions of the same-currency gross holdings value.
pub fn holding_risk(bars: &[Bar], weight: Option<f64>, threshold: f64) -> HoldingRisk {
    let closes: Vec<_> = bars.iter().map(|b| b.close).collect();
    HoldingRisk {
        above_sma50: sma50_stretch(bars).map(|s| s.pct >= 0.0),
        rsi14: rsi(&closes, 14).last().copied().filter(|v| v.is_finite()),
        concentrated: weight
            .filter(|v| v.is_finite() && *v >= 0.0)
            .filter(|_| threshold.is_finite() && (0.0..=1.0).contains(&threshold))
            .map(|v| v > threshold),
    }
}

/// Daily close/SPY close ratio, normalized to 100 on the first common valid UTC
/// date. No forward filling. Output is aligned to `bars`, with NaN for gaps.
pub fn daily_relative_strength(bars: &[Bar], spy: &[Bar]) -> Vec<f64> {
    let benchmark: std::collections::BTreeMap<_, _> = spy.iter().map(|b| (b.ts.div_euclid(86400), b.close)).collect();
    let mut first = None;
    bars.iter()
        .map(|b| {
            let ratio = benchmark
                .get(&b.ts.div_euclid(86400))
                .filter(|s| s.is_finite() && **s > 0.0)
                .map(|s| b.close / s)
                .filter(|r| r.is_finite() && *r > 0.0);
            match ratio {
                Some(r) => r / *first.get_or_insert(r) * 100.0,
                None => f64::NAN,
            }
        })
        .collect()
}

/// Relative return over exactly `period` symbol sessions, requiring matching SPY
/// observations at both endpoints. Positive means outperformance.
pub fn daily_relative_return(bars: &[Bar], spy: &[Bar], period: usize) -> Option<f64> {
    if period == 0 || bars.len() <= period {
        return None;
    }
    let line = daily_relative_strength(bars, spy);
    let end = line.len() - 1;
    let result = (line[end] / line[end - period] - 1.0) * 100.0;
    result.is_finite().then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bars(n: usize, price: f64) -> Vec<Bar> {
        (0..n).map(|i| Bar::new(i as i64 * 86400, price, price + 1.0, price - 1.0, price, 1.0)).collect()
    }
    #[test]
    fn breaches_count_each_holding_once_and_ignore_unknowns() {
        let unknown = HoldingRisk { above_sma50: None, rsi14: None, concentrated: None };
        assert_eq!(risk_breaches([]), 0);
        assert_eq!(risk_breaches([unknown]), 0);
        assert_eq!(risk_breaches([
            HoldingRisk { above_sma50: Some(false), rsi14: Some(70.0), concentrated: Some(true) },
            HoldingRisk { rsi14: Some(30.0), ..unknown },
            HoldingRisk { concentrated: Some(true), ..unknown },
            HoldingRisk { above_sma50: Some(true), rsi14: Some(50.0), concentrated: Some(false) },
            HoldingRisk { rsi14: Some(f64::NAN), ..unknown },
        ]), 3);
    }
    #[test]
    fn stretch_uses_full_sma50_and_wilder_atr() {
        assert!(sma50_stretch(&bars(49, 100.0)).is_none());
        let mut b = bars(50, 100.0);
        b[49].close = 110.0;
        b[49].high = 111.0;
        let s = sma50_stretch(&b).unwrap();
        assert!((s.pct - (110.0 / 100.2 - 1.0) * 100.0).abs() < 1e-9);
        assert!((s.atr_units.unwrap() - 9.8 / (2.0 + 10.0 / 14.0)).abs() < 1e-9);
        let flat: Vec<_> = (0..50).map(|i| Bar::new(i * 86400, 1.0, 1.0, 1.0, 1.0, 0.0)).collect();
        assert_eq!(sma50_stretch(&flat).unwrap().atr_units, None);
    }
    #[test]
    fn holding_flags_preserve_missing_values_and_strict_concentration() {
        let r = holding_risk(&bars(50, 100.0), Some(0.2), 0.2);
        assert_eq!(r.above_sma50, Some(true));
        assert_eq!(r.rsi14, Some(50.0));
        assert_eq!(r.concentrated, Some(false));
        assert_eq!(holding_risk(&[], Some(0.21), 0.2).concentrated, Some(true));
        let mut up = bars(50, 100.0);
        for (i, b) in up.iter_mut().enumerate() {
            b.close += i as f64;
        }
        let r = holding_risk(&up, Some(f64::NAN), 0.2);
        assert_eq!(r.rsi14, Some(100.0));
        assert_eq!(r.above_sma50, Some(true));
        assert_eq!(r.concentrated, None);
        for (i, b) in up.iter_mut().enumerate() {
            b.close = 100.0 - i as f64;
        }
        let r = holding_risk(&up, Some(0.1), f64::NAN);
        assert_eq!(r.rsi14, Some(0.0));
        assert_eq!(r.above_sma50, Some(false));
        assert_eq!(r.concentrated, None);
        let r = holding_risk(&[], None, 0.2);
        assert_eq!(r.above_sma50, None);
        assert_eq!(r.rsi14, None);
        assert_eq!(r.concentrated, None);
    }
    #[test]
    fn relative_line_aligns_dates_normalizes_and_leaves_gaps() {
        let mut b = bars(3, 100.0);
        b[2].close = 120.0;
        let mut spy = bars(3, 200.0);
        spy[2].close = 220.0;
        spy.remove(1);
        for s in &mut spy {
            s.ts += 3600;
        }
        let r = daily_relative_strength(&b, &spy);
        assert_eq!(r[0], 100.0);
        assert!(r[1].is_nan());
        assert!((r[2] - 109.0909090909).abs() < 1e-8);
        assert!(daily_relative_strength(&b, &[]).iter().all(|v| v.is_nan()));
    }
    #[test]
    fn relative_return_requires_exact_endpoints() {
        let mut b = bars(64, 100.0);
        b[63].close = 110.0;
        let spy = bars(64, 200.0);
        assert!((daily_relative_return(&b, &spy, 63).unwrap() - 10.0).abs() < 1e-9);
        assert!(daily_relative_return(&b, &spy[..63], 63).is_none());
        assert!(daily_relative_return(&b[..63], &spy, 63).is_none());
    }
}
