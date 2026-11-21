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

/// ADMA(er=20, fast=2, slow=60) signed stretch in percent.
/// Source: /workspace/codex-runs/wn60/adaptive_trend.py::_adaptive_ma and
/// /workspace/codex-runs/wn60/equity_engine.py::adma_stretch.
pub fn adma_stretch(close: &[f64]) -> Option<f64> {
    if close.iter().filter(|v| !v.is_nan()).count() < 40 {
        return None;
    }
    let ma = adaptive_ma_adma(close);
    // pandas dropna excludes NaN, but retains infinities; _finite is applied afterwards.
    let last_ma = *ma.iter().rev().find(|v| !v.is_nan())?;
    let last = *close.last()?;
    if !last.is_finite() || !last_ma.is_finite() || last_ma <= 0.0 {
        return None;
    }
    let pct = (last / last_ma - 1.0) * 100.0;
    // Decimal formatting uses ties-to-even, as Python round(value, 2) does.
    pct.is_finite().then(|| format!("{pct:.2}").parse().unwrap())
}

/// Line-for-line ADMA branch of wn60/adaptive_trend.py::_adaptive_ma.
fn adaptive_ma_adma(prices: &[f64]) -> Vec<f64> {
    let fast_sc = 2.0 / 3.0;
    let slow_sc = 2.0 / 61.0;
    let n = prices.len();
    let mut out = vec![f64::NAN; n];
    let Some(first) = prices.iter().position(|v| v.is_finite()) else { return out; };
    let seed = first + 20;
    if seed >= n { return out; }
    out[seed - 1] = prices[seed - 1];
    for i in seed..n {
        if !prices[i].is_finite() {
            out[i] = if out[i - 1].is_finite() { out[i - 1] } else { f64::NAN };
            continue;
        }
        let direction = (prices[i] - prices[i - 20]).abs();
        let window = &prices[i - 20..=i];
        let volatility: f64 = window.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
        let er = if volatility > 1e-12 { direction / volatility } else { 0.0 };
        let alpha = slow_sc + er * (fast_sc - slow_sc);
        // Python max(min(alpha, 1), 0) preserves a NaN first argument.
        let alpha = if alpha > 1.0 { 1.0 } else { alpha };
        let alpha = if alpha < 0.0 { 0.0 } else { alpha };
        out[i] = out[i - 1] + alpha * (prices[i] - out[i - 1]);
    }
    out
}

#[cfg(test)]
mod adma_tests {
    use super::adma_stretch;

    // Generated with PYTHONDONTWRITEBYTECODE=1 python3, importing equity_engine
    // from /workspace/codex-runs/wn60 (read-only), calling adma_stretch and
    // adaptive_trend._adaptive_ma(pd.Series(values), 20, 2, 60, method="adma").
    #[test]
    fn whats_news_python_parity_with_gaps() {
        let base = vec![
            100.0, 102.3, 104.6, 106.9, 109.2, 111.5, 113.8,
            102.1, 104.4, 106.7, 109.0, 111.3, 113.6, 115.9,
            104.2, 106.5, 108.8, 111.1, 113.4, 115.7, 118.0,
            106.3, 108.6, 110.9, 113.2, 115.5, 117.8, 120.1,
            108.4, 110.7, 113.0, 115.3, 117.6, 119.9, 122.2,
            110.5, 112.8, 115.1, 117.4, 119.7, 122.0, 124.3,
            112.6, 114.9, 117.2, 119.5, 121.8, 124.1, 126.4,
            114.7, 117.0, 119.3, 121.6, 123.9, 126.2, 128.5,
            116.8, 119.1, 121.4, 123.7, 126.0, 128.3, 130.6,
            118.9, 121.2,
        ];
        let mut middle = base.clone(); middle[30] = f64::NAN;
        let mut leading = vec![f64::NAN; 4]; leading.extend_from_slice(&base);
        let mut trailing = base.clone(); trailing.push(f64::NAN);
        let mut seed = base.clone(); seed[19] = f64::NAN;
        let mut infinity = base.clone(); infinity[30] = f64::INFINITY;
        let cases = [
            ("regular", base.clone(), Some(-1.95), 123.60811675134747),
            ("middle gap", middle, Some(-1.33), 122.83224337055219),
            ("leading gap", leading, Some(-1.95), 123.60811675134747),
            ("trailing gap", trailing, None, 123.60811675134747),
            ("seed gap", seed, None, f64::NAN),
            ("short", base[..39].to_vec(), None, 116.2552562522923),
            ("infinity", infinity, Some(3.79), f64::NAN),
            ("flat", vec![10.0; 40], Some(0.0), 10.0),
            ("zero MA", vec![0.0; 40], None, 0.0),
        ];
        for (name, prices, expected, expected_ma) in cases {
            assert_eq!(adma_stretch(&prices), expected, "{name}");
            let ma = super::adaptive_ma_adma(&prices);
            let last = *ma.last().unwrap();
            assert!(if expected_ma.is_nan() { last.is_nan() } else { (last - expected_ma).abs() < 1e-10 }, "{name}: {last}");
        }
        assert_eq!(adma_stretch(&vec![f64::NAN; 50]), None);
        assert!(super::adaptive_ma_adma(&[10.0; 20]).iter().all(|v| v.is_nan()));
    }

    // Reference seed and unsquared alpha: wn60/adaptive_trend.py::_adaptive_ma.
    #[test]
    fn reference_seed_and_unsquared_alpha() {
        // Whats-News requires at least 40 closes.
        assert_eq!(adma_stretch(&vec![10.0; 39]), None);
        let mut prices = vec![10.0; 39];
        prices.push(13.0);
        // ER = 3/3 = 1 → alpha = fast = 2/3; MA = 10 + 2/3*3 = 12.
        assert!((adma_stretch(&prices).unwrap() - 8.33).abs() < 1e-10);
        assert_eq!(adma_stretch(&vec![10.0; 40]), Some(0.0));
        prices.push(f64::NAN);
        assert_eq!(adma_stretch(&prices), None);
    }
}
