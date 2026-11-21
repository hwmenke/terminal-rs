//! Deterministic synthetic OHLCV data, used when no market-data key is
//! configured and for benchmarks. The same symbol always yields the same path.

use crate::{Bar, Timeframe};

/// Small, fast xorshift64* PRNG so sample data needs no external crates.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal via Box-Muller.
    pub fn normal(&mut self) -> f64 {
        let u1 = self.f64().max(1e-12);
        let u2 = self.f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// FNV-1a hash used to seed per-symbol paths.
pub fn seed_for(symbol: &str) -> u64 {
    symbol.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// Generate `count` bars ending at (and including the bucket of) `end_ts`.
pub fn generate(symbol: &str, tf: Timeframe, count: usize, end_ts: i64) -> Vec<Bar> {
    generate_with(symbol, tf, count, end_ts, 20.0 + (seed_for(symbol) % 480) as f64, 0.30)
}

/// Like [`generate`] but for a security: the path ends near `level` (e.g. 65,000 for
/// BTCUSD, 1.09 for EURUSD, 4.2 for a 10Y yield) with the asset class's typical volatility.
pub fn for_security(sec: &crate::Security, tf: Timeframe, count: usize, end_ts: i64) -> Vec<Bar> {
    let level = crate::catalog::by_security(sec).map(|l| l.sample_level);
    let level = level.unwrap_or(20.0 + (seed_for(&sec.ticker) % 480) as f64);
    let mut bars = generate_with(&sec.key(), tf, count, end_ts, 1.0, sec.class.sample_vol());
    // Rescale so the latest close sits at the instrument's typical level.
    if let Some(last) = bars.last().map(|b| b.close) {
        let k = level / last;
        for b in &mut bars {
            b.open *= k;
            b.high *= k;
            b.low *= k;
            b.close *= k;
        }
    }
    bars
}

pub fn generate_with(
    symbol: &str,
    tf: Timeframe,
    count: usize,
    end_ts: i64,
    start_price: f64,
    annual_vol: f64,
) -> Vec<Bar> {
    let mut rng = Rng::new(seed_for(symbol) ^ tf.seconds() as u64);
    let step = tf.seconds();
    let end = tf.bucket_start(end_ts);
    let sigma = annual_vol * (step as f64 / (365.0 * 86_400.0)).sqrt();
    let drift = 0.08 * step as f64 / (365.0 * 86_400.0);
    let mut price = start_price;
    let base_vol = 1.0e6 * (step as f64 / 86_400.0).sqrt();
    let mut bars = Vec::with_capacity(count);
    for i in 0..count {
        let ts = match tf {
            // Calendar buckets: step back roughly then snap so weeks/months align.
            Timeframe::W1 | Timeframe::MN1 => tf.bucket_start(end - (count - 1 - i) as i64 * step),
            _ => end - (count - 1 - i) as i64 * step,
        };
        let open = price;
        let ret = drift + sigma * rng.normal();
        let close = (open * ret.exp()).max(0.01);
        let wick = sigma * open * 0.6;
        let high = open.max(close) + wick * rng.f64();
        let low = (open.min(close) - wick * rng.f64()).max(0.005);
        let volume = base_vol * (0.5 + rng.f64()) * (1.0 + 20.0 * ret.abs() / sigma.max(1e-9) * 0.05);
        bars.push(Bar::new(ts, open, high, low, close, volume.round()).with_provenance(crate::Provenance::Sample));
        price = close;
    }
    bars.dedup_by_key(|b| b.ts);
    bars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_valid() {
        let a = generate("AAPL", Timeframe::D1, 500, 1_790_000_000);
        let b = generate("AAPL", Timeframe::D1, 500, 1_790_000_000);
        assert_eq!(a, b);
        assert_eq!(a.len(), 500);
        assert!(a.windows(2).all(|w| w[0].ts < w[1].ts));
        assert!(a.iter().all(|b| b.low <= b.open.min(b.close) && b.high >= b.open.max(b.close) && b.low > 0.0));
        assert_ne!(a, generate("MSFT", Timeframe::D1, 500, 1_790_000_000));
    }

    #[test]
    fn security_levels() {
        use crate::{AssetClass, Security};
        let btc = for_security(&Security::new("BTCUSD", AssetClass::Crypto), Timeframe::D1, 300, 1_790_000_000);
        assert!((btc.last().unwrap().close - 65_000.0).abs() < 1e-6);
        let tnx = for_security(&Security::new("US10Y", AssetClass::Govt), Timeframe::D1, 300, 1_790_000_000);
        assert!(tnx.iter().all(|b| b.low > 0.0 && b.high < 20.0));
        let fx = for_security(&Security::new("EURUSD", AssetClass::Fx), Timeframe::H1, 300, 1_790_000_000);
        assert!(fx.iter().all(|b| (b.close - 1.09).abs() < 0.5));
    }

    #[test]
    fn monthly_buckets_are_unique() {
        let m = generate("SPY", Timeframe::MN1, 120, 1_790_000_000);
        assert!(m.windows(2).all(|w| w[0].ts < w[1].ts));
        assert!(m.len() >= 110);
    }
}
