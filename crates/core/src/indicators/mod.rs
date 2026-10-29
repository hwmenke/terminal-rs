//! Technical indicators. Every function returns a vector aligned with its input; positions
//! without enough history hold `f64::NAN`. Inputs may themselves start with NaN (the output
//! of another indicator): smoothing starts at the first defined value, and a gap later on
//! blanks only the windows that contain it.
//!
//! Conventions follow the common charting definitions: EMA uses alpha = 2/(n+1) seeded with
//! an SMA, RSI, ATR and ADX use Wilder smoothing, standard deviations are population ones,
//! and VWAP and pivot levels reset each session (a New York calendar day intraday, a month
//! on daily bars, a year above that).
//!
//! The functions are grouped by family; [`registry`] lists every indicator with its
//! parameters, placement and outputs, and is what the app's command line, picker and chart
//! are driven from.

use crate::Bar;

mod adaptive;
mod levels;
mod ma;
mod momentum;
pub mod registry;
mod stats;
mod trend;
mod volatility;
mod volume;

pub use adaptive::*;
pub use levels::*;
pub use ma::*;
pub use momentum::*;
pub use stats::*;
pub use trend::*;
pub use volatility::*;
pub use volume::*;

const NAN: f64 = f64::NAN;

/// Bars split into columns: the input of the bar-based indicators.
#[derive(Debug, Clone, Default)]
pub struct Ohlcv {
    pub ts: Vec<i64>,
    pub open: Vec<f64>,
    pub high: Vec<f64>,
    pub low: Vec<f64>,
    pub close: Vec<f64>,
    pub volume: Vec<f64>,
}

impl Ohlcv {
    pub fn new(bars: &[Bar]) -> Self {
        Self {
            ts: bars.iter().map(|b| b.ts).collect(),
            open: bars.iter().map(|b| b.open).collect(),
            high: bars.iter().map(|b| b.high).collect(),
            low: bars.iter().map(|b| b.low).collect(),
            close: bars.iter().map(|b| b.close).collect(),
            volume: bars.iter().map(|b| b.volume).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.close.len()
    }

    pub fn is_empty(&self) -> bool {
        self.close.is_empty()
    }

    /// Median price, (high + low) / 2.
    pub fn hl2(&self) -> Vec<f64> {
        zip(&self.high, &self.low, |h, l| (h + l) / 2.0)
    }

    /// Typical price, (high + low + close) / 3.
    pub fn hlc3(&self) -> Vec<f64> {
        (0..self.len()).map(|i| (self.high[i] + self.low[i] + self.close[i]) / 3.0).collect()
    }

    /// Average price, (open + high + low + close) / 4.
    pub fn ohlc4(&self) -> Vec<f64> {
        (0..self.len()).map(|i| (self.open[i] + self.high[i] + self.low[i] + self.close[i]) / 4.0).collect()
    }

    /// Weighted close, (high + low + 2 × close) / 4.
    pub fn hlcc4(&self) -> Vec<f64> {
        (0..self.len()).map(|i| (self.high[i] + self.low[i] + 2.0 * self.close[i]) / 4.0).collect()
    }
}

/// Run `f` on `values` from the first finite one on; leading NaNs stay NaN.
fn from_first(values: &[f64], f: impl FnOnce(&[f64]) -> Vec<f64>) -> Vec<f64> {
    let start = values.iter().position(|v| v.is_finite()).unwrap_or(values.len());
    let mut out = vec![NAN; start];
    out.extend(f(&values[start..]));
    debug_assert_eq!(out.len(), values.len());
    out
}

fn zip(a: &[f64], b: &[f64], f: impl Fn(f64, f64) -> f64) -> Vec<f64> {
    a.iter().zip(b).map(|(x, y)| f(*x, *y)).collect()
}

/// `a / b`, or `zero` when `b` is 0.
fn ratio(a: f64, b: f64, zero: f64) -> f64 {
    if b == 0.0 { zero } else { a / b }
}

/// `values[i - k]`, NaN before the start.
fn ago(values: &[f64], i: usize, k: usize) -> f64 {
    if i >= k { values[i - k] } else { NAN }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close_to(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn assert_series(got: &[f64], want: &[f64]) {
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert!((g.is_nan() && w.is_nan()) || close_to(*g, *w), "index {i}: got {g}, want {w}");
        }
    }

    const N: f64 = f64::NAN;

    #[test]
    fn sma_basic() {
        assert_series(&sma(&[1.0, 2.0, 3.0, 4.0, 5.0], 3), &[N, N, 2.0, 3.0, 4.0]);
        assert!(sma(&[1.0], 3)[0].is_nan());
        assert!(sma(&[1.0, 2.0], 0).iter().all(|v| v.is_nan()));
    }

    #[test]
    fn ema_seeded_with_sma() {
        // alpha = 0.5; seed = mean(1,2,3) = 2; then 0.5*4+0.5*2 = 3; 0.5*5+0.5*3 = 4.
        assert_series(&ema(&[1.0, 2.0, 3.0, 4.0, 5.0], 3), &[N, N, 2.0, 3.0, 4.0]);
        // Constant input stays constant.
        assert!(close_to(ema(&[7.0; 50], 10)[49], 7.0));
        // Leading NaNs (another indicator's warm-up) delay the seed instead of poisoning it.
        assert_series(&ema(&[N, N, 1.0, 2.0, 3.0, 4.0], 3), &[N, N, N, N, 2.0, 3.0]);
    }

    #[test]
    fn rsi_reference_values() {
        // Wilder's original 14-period example (New Concepts in Technical Trading Systems).
        let closes = [
            44.34, 44.09, 44.15, 43.61, 44.33, 44.83, 45.10, 45.42, 45.84, 46.08, 45.89, 46.03, 45.61, 46.28, 46.28,
            46.00, 46.03, 46.41, 46.22, 45.64,
        ];
        let r = rsi(&closes, 14);
        assert!(r[13].is_nan());
        assert!((r[14] - 70.46).abs() < 0.01, "rsi[14] = {}", r[14]);
        assert!((r[15] - 66.25).abs() < 0.01, "rsi[15] = {}", r[15]);
        assert!((r[19] - 57.92).abs() < 0.05, "rsi[19] = {}", r[19]);
    }

    #[test]
    fn rsi_extremes() {
        let up: Vec<f64> = (0..30).map(|i| i as f64).collect();
        assert_eq!(rsi(&up, 14)[29], 100.0);
        assert_eq!(rsi(&[5.0; 30], 14)[29], 50.0);
    }

    #[test]
    fn macd_matches_manual_emas() {
        let closes: Vec<f64> = (0..60).map(|i| 100.0 + (i as f64 * 0.3).sin() * 5.0 + i as f64 * 0.1).collect();
        let m = macd(&closes, 12, 26, 9);
        let f = ema(&closes, 12);
        let s = ema(&closes, 26);
        assert!(m.macd[24].is_nan());
        assert!(close_to(m.macd[25], f[25] - s[25]));
        // Signal needs 9 MACD values: first at 25 + 8.
        assert!(m.signal[32].is_nan());
        let seed: f64 = m.macd[25..34].iter().sum::<f64>() / 9.0;
        assert!(close_to(m.signal[33], seed));
        assert!(close_to(m.histogram[40], m.macd[40] - m.signal[40]));
    }

    #[test]
    fn bollinger_population_sd() {
        let b = bollinger(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0], 8, 2.0);
        // mean 5, population sd 2.
        assert!(close_to(b.middle[7], 5.0));
        assert!(close_to(b.upper[7], 9.0));
        assert!(close_to(b.lower[7], 1.0));
        assert!(b.upper[6].is_nan());
        // Rolling window stays numerically consistent with a fresh computation.
        let xs: Vec<f64> = (0..200).map(|i| ((i * 37) % 101) as f64).collect();
        let roll = bollinger(&xs, 20, 2.0);
        let fresh = bollinger(&xs[180..], 20, 2.0);
        assert!(close_to(roll.upper[199], fresh.upper[19]));
    }

    #[test]
    fn vwap_resets_per_session() {
        let day = 86_400;
        let bars = [
            Bar::new(0, 0.0, 12.0, 8.0, 10.0, 100.0),
            Bar::new(60, 0.0, 22.0, 18.0, 20.0, 300.0),
            Bar::new(day, 0.0, 6.0, 4.0, 5.0, 10.0),
        ];
        let v = vwap(&bars);
        assert!(close_to(v[0], 10.0));
        assert!(close_to(v[1], (10.0 * 100.0 + 20.0 * 300.0) / 400.0));
        assert!(close_to(v[2], 5.0));
    }
}
