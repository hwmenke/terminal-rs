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

/// Matching coverage receipt for the dev-only cash fixture.
pub fn stitch(bars: &[Bar], tf: Timeframe) -> tr_data::vault::StitchInfo {
    let mut dates = bars.iter().map(|b| tr_core::ny::civil(b.ts).date()).collect::<Vec<_>>();
    dates.dedup();
    let at = bars.last().unwrap().ts + tf.seconds();
    let cells =
        dates.into_iter().map(|date| tr_data::vault::merge::classify_session(date, tf, bars, 1, 0, at)).collect();
    tr_data::vault::StitchInfo {
        cells,
        source: "benchmark fixture only".into(),
        epoch: 1,
        vault_bars: bars.len(),
        sessions: 20,
        live_ok: true,
        fetched_at: Some(at),
        ..Default::default()
    }
}

/// Dev-only OR admission fixture: all three states recur on native cash buckets.
/// Deliberately bounded moves keep the opening range distinct from later states.
pub fn or_lens(sessions: usize) -> Vec<Bar> {
    let mut date = "2026-01-05".parse().unwrap();
    let mut out = Vec::new();
    for s in 0..sessions {
        let mut prior: f64 = 100.;
        for (k, ts) in intraday::regular_opens(date, Timeframe::M30).into_iter().enumerate() {
            let c =
                if k == 0 { 100. } else { [101., 99., 100.][s % 3] + 0.06 * (s as f64 * 0.7 + k as f64 * 0.6).sin() };
            let high = if k == 0 { 100.25 } else { prior.max(c) + 0.08 };
            let low = if k == 0 { 99.75 } else { prior.min(c) - 0.08 };
            out.push(Bar::new(ts, prior, high, low, c, 1000.).with_provenance(Provenance::Real));
            prior = c;
        }
        date = calendar::next_session(date);
    }
    out
}

/// Dev-only 5m evidence fixture; 120 full native sessions, varying clock effects.
pub fn season_evidence(sessions: usize) -> Vec<Bar> {
    let mut date = "2026-01-05".parse().unwrap();
    let mut out = Vec::new();
    for s in 0..sessions {
        for (k, ts) in intraday::regular_opens(date, Timeframe::M5).into_iter().enumerate() {
            let effect = 0.0002 * (k as f64 * 0.15).cos();
            let noise = 0.001 * (s as f64 * 1.7 + k as f64 * 0.3).sin();
            let close = 100. * (1. + effect + noise);
            out.push(Bar::new(ts, 100., 100.2, 99.8, close, 1000. + k as f64).with_provenance(Provenance::Real));
        }
        date = calendar::next_session(date);
    }
    out
}

/// Verified flags are exclusively for exercising admission in offline benchmarks.
pub fn relationship(n: usize) -> (Vec<Bar>, Vec<Bar>) {
    (daily(n + 1, 71), daily(n + 1, 73))
}

/// A rectangular 78-clock session grid with nondegenerate common-row variation.
pub fn inference_clocks(sessions: usize) -> Vec<Vec<Option<f64>>> {
    let mut rng = Rng(311);
    let common: Vec<_> = (0..sessions).map(|s| 0.0003 * (s as f64 * 0.3).sin() + 0.0007 * (rng.next() - 0.5)).collect();
    (0..78)
        .map(|k| (0..sessions).map(|s| Some(common[s] + 0.0005 * (rng.next() - 0.5) + k as f64 * 0.000001)).collect())
        .collect()
}

/// Aperiodic volatility blocks, exclusively for offline regime admissions.
pub fn regime_walk(n: usize) -> Vec<Bar> {
    let mut bars = daily(n + 1, 42);
    let mut rng = Rng(42);
    let mut left = 0;
    let mut level = 1.;
    let mut price = 100.;
    for (i, b) in bars.iter_mut().enumerate() {
        let open = price;
        if i > 0 {
            if left == 0 {
                left = 40 + (121. * rng.next()).floor() as usize;
                level = [0.4, 1., 2.5][(3. * rng.next()).floor() as usize];
            }
            price *= (level * 0.006 * (rng.next() - 0.5) + 0.0001).exp();
            left -= 1;
        }
        *b = Bar::new(b.ts, open, open.max(price), open.min(price), price, 1000.).with_provenance(Provenance::Real);
    }
    bars
}

/// Shell routing inputs only: the same validated routes as the shipped command parser.
pub fn studio_routes() -> [&'static str; 5] {
    ["5m lens=dist", "5m lens=relate benchmark=QQQ", "5m lens=when", "5m lens=season", "5m lens=backtest"]
}

/// Dev-only When input: AAPL 5m regular sessions (78 bars each) with a
/// matching observation clock. Fabricated benchmark input only.
pub fn when_input(sessions: usize) -> tr_data::studio_canvas::CanvasInput {
    let bars = intraday(Timeframe::M5, sessions, 907);
    tr_data::studio_canvas::CanvasInput {
        symbol: "AAPL US Equity".into(),
        tf: Timeframe::M5,
        observed_at: Some(bars.last().unwrap().ts + 300),
        stitch: None,
        source: "benchmark fixture only".into(),
        bars: std::sync::Arc::new(bars),
    }
}

/// Four distinct admitted native sources, 7,800 bars apiece at full scale.
/// Fabricated benchmark inputs only; never a runtime dependency of the app.
pub fn canvas_inputs(sessions:usize)->Vec<tr_data::studio_canvas::CanvasInput> {
    ["AAPL","QQQ","MSFT","SPY"].into_iter().enumerate().map(|(i,symbol)|{
        let bars=intraday(Timeframe::M1,sessions,701+i as u64);
        tr_data::studio_canvas::CanvasInput {symbol:format!("{symbol} US Equity"),tf:Timeframe::M1,observed_at:Some(bars.last().unwrap().ts+60),stitch:None,source:"benchmark fixture only".into(),bars:std::sync::Arc::new(bars)}
    }).collect()
}
pub fn canvas_events(inputs:&[tr_data::studio_canvas::CanvasInput],n:usize)->Vec<tr_data::studio_canvas::CanvasEvent> {
    use tr_data::studio_canvas::{CanvasEvent,EventTime,EventKind};
    let bars=&inputs[0].bars;
    (0..n).map(|i|CanvasEvent {kind:if i%9==0 {EventKind::Earnings}else{EventKind::News},
        time:if i%9==0 {let date=tr_core::ny::civil(bars[i*bars.len()/n].ts).date();EventTime::Date {start:date,end:date}}else{EventTime::Instant(bars[i*bars.len()/n].ts+15)},
        source:"benchmark event fixture only".into(),headline:format!("Fixture record {i}")}).collect()
}
