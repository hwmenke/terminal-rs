//! Offline benchmark fixtures only; generated bars never enter the application.
use std::collections::BTreeMap;
use tr_core::{Bar, Provenance, Timeframe};
use tr_data::{
    backtest_honesty::{Reason, ReturnPoint},
    history::calendar,
    intraday,
};
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }
}
fn walk(times: impl IntoIterator<Item = i64>, seed: u64, intraday: bool) -> Vec<Bar> {
    let mut rng = Rng(seed);
    let mut price = 100.0;
    times
        .into_iter()
        .enumerate()
        .map(|(i, ts)| {
            let open = price;
            let drift = 0.0003 + 0.0015 * (i as f64 * 0.013).sin();
            price *= (drift + (rng.next() - 0.5) * if intraday { 0.008 } else { 0.035 }).exp();
            let spread = price * (0.001 + rng.next() * 0.003);
            let volume = 1000.0
                * (1.0 + 2.0 * (std::f64::consts::PI * (i % 390) as f64 / 390.0).cos().powi(2))
                * (0.8 + rng.next() * 0.4);
            Bar::new(ts, open, open.max(price) + spread, open.min(price) - spread, price, volume)
                .with_provenance(Provenance::Real)
        })
        .collect()
}
pub fn daily(n: usize, seed: u64) -> Vec<Bar> {
    let mut date = "2026-10-30".parse().unwrap();
    let mut dates = Vec::with_capacity(n);
    for _ in 0..n {
        dates.push(date);
        date = calendar::previous_session(date);
    }
    dates.reverse();
    walk(dates.into_iter().map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp()), seed, false)
}
pub fn intraday(tf: Timeframe, sessions: usize, seed: u64) -> Vec<Bar> {
    let mut date = "2026-08-03".parse().unwrap();
    let mut times = vec![];
    for _ in 0..sessions {
        times.extend(intraday::regular_opens(date, tf));
        date = calendar::next_session(date);
    }
    walk(times, seed, true)
}
pub fn crypto_m5(n: usize, end_ts: i64) -> Vec<Bar> {
    walk((0..n).map(|i| end_ts - (n - 1 - i) as i64 * 300), 42, true)
}
pub fn returns_family(trials: usize, n: usize, seed: u64) -> BTreeMap<String, Result<Vec<ReturnPoint>, Reason>> {
    let bars = daily(n, seed);
    let mut rng = Rng(seed);
    (0..trials)
        .map(|trial| {
            let tape = bars
                .iter()
                .enumerate()
                .map(|(i, b)| ReturnPoint {
                    session_date: chrono::DateTime::from_timestamp(b.ts, 0).unwrap().date_naive().to_string(),
                    period_start: b.ts,
                    period_end: b.ts + 86400,
                    net_return: 0.0001
                        + 0.008 * (i as f64 * 0.19 + (trial % 8) as f64).sin()
                        + 0.012 * (rng.next() - 0.5),
                })
                .collect();
            (format!("trial{trial}"), Ok(tape))
        })
        .collect()
}
