//! Volume: cumulative flows, money flow, volume oscillators and session VWAP.

use crate::Bar;

use super::ma::{ema, rma, sma};
use super::momentum::{Macd, ppo, roc};
use super::stats::{change, sum};
use super::volatility::Bands;
use super::{NAN, Ohlcv, ago, ratio, zip};

/// VWAP resetting whenever `session_of(ts)` changes; falls back to the typical price
/// while the session has only reported zero volume. Missing observations are NaN
/// and excluded from subsequent weighted totals.
pub fn vwap_by(bars: &[Bar], session_of: impl Fn(i64) -> i64) -> Vec<f64> {
    let mut out = Vec::with_capacity(bars.len());
    let (mut pv, mut vol) = (0.0, 0.0);
    let mut session = None;
    for b in bars {
        let s = session_of(b.ts);
        if session != Some(s) {
            session = Some(s);
            pv = 0.0;
            vol = 0.0;
        }
        if b.volume_value().is_none() {
            out.push(NAN);
            continue;
        }
        pv += b.hlc3() * b.volume;
        vol += b.volume;
        out.push(if vol > 0.0 { pv / vol } else { b.hlc3() });
    }
    out
}

/// Session VWAP resetting each New York calendar day. For daily and longer bars this
/// degenerates to hlc3, so callers typically anchor it by month instead.
pub fn vwap(bars: &[Bar]) -> Vec<f64> {
    vwap_by(bars, crate::ny::date_key)
}

/// Session VWAP with bands `k` volume-weighted standard deviations of the typical price
/// away. Missing volume leaves a gap and is excluded from weighted totals.
pub fn vwap_bands(o: &Ohlcv, session_of: impl Fn(i64) -> i64, k: f64) -> Bands {
    let tp = o.hlc3();
    let len = o.len();
    let (mut middle, mut width) = (vec![NAN; len], vec![NAN; len]);
    let (mut pv, mut pv2, mut vol, mut session) = (0.0, 0.0, 0.0, None);
    for i in 0..len {
        let s = session_of(o.ts[i]);
        if session != Some(s) {
            (pv, pv2, vol, session) = (0.0, 0.0, 0.0, Some(s));
        }
        let v = o.volume[i];
        if !v.is_finite() || v < 0.0 {
            continue;
        }
        pv += tp[i] * v;
        pv2 += tp[i] * tp[i] * v;
        vol += v;
        if vol > 0.0 {
            let m = pv / vol;
            middle[i] = m;
            width[i] = k * (pv2 / vol - m * m).max(0.0).sqrt();
        } else {
            (middle[i], width[i]) = (tp[i], 0.0);
        }
    }
    let upper = zip(&middle, &width, |m, w| m + w);
    let lower = zip(&middle, &width, |m, w| m - w);
    Bands { upper, middle, lower }
}

/// Running total of `f(i)` from the first bar.
fn cumulative(len: usize, mut f: impl FnMut(usize) -> f64) -> Vec<f64> {
    let mut acc = 0.0;
    (0..len)
        .map(|i| {
            acc += f(i);
            acc
        })
        .collect()
}

/// `values[i] - values[i - 1]` with 0 on the first bar.
fn step(values: &[f64], i: usize) -> f64 {
    if i == 0 { 0.0 } else { values[i] - values[i - 1] }
}

/// On-balance volume: volume added on up closes, subtracted on down closes.
pub fn obv(o: &Ohlcv) -> Vec<f64> {
    cumulative(o.len(), |i| {
        let d = step(&o.close, i);
        if !o.volume[i].is_finite() {
            NAN
        } else if d > 0.0 {
            o.volume[i]
        } else if d < 0.0 {
            -o.volume[i]
        } else {
            0.0
        }
    })
}

/// Close location value × volume: the money flow volume of each bar.
fn money_flow_volume(o: &Ohlcv) -> Vec<f64> {
    (0..o.len())
        .map(|i| {
            let (h, l, c) = (o.high[i], o.low[i], o.close[i]);
            ratio((c - l) - (h - c), h - l, 0.0) * o.volume[i]
        })
        .collect()
}

/// Chaikin's accumulation/distribution line.
pub fn adl(o: &Ohlcv) -> Vec<f64> {
    let mfv = money_flow_volume(o);
    cumulative(o.len(), |i| mfv[i])
}

/// Chaikin money flow: money flow volume over volume across `n` bars, −1..1.
pub fn cmf(o: &Ohlcv, n: usize) -> Vec<f64> {
    zip(&sum(&money_flow_volume(o), n), &sum(&o.volume, n), |m, v| ratio(m, v, 0.0))
}

/// Chaikin oscillator: EMA(ADL, `fast`) − EMA(ADL, `slow`).
pub fn chaikin_osc(o: &Ohlcv, fast: usize, slow: usize) -> Vec<f64> {
    let a = adl(o);
    zip(&ema(&a, fast), &ema(&a, slow), |f, s| f - s)
}

/// Money flow index: a volume-weighted RSI of the typical price, 0..100.
pub fn mfi(o: &Ohlcv, n: usize) -> Vec<f64> {
    let tp = o.hlc3();
    let flow = |up: bool| -> Vec<f64> {
        (0..o.len())
            .map(|i| {
                let d = tp[i] - ago(&tp, i, 1);
                if d.is_nan() || !o.volume[i].is_finite() {
                    NAN
                } else if (up && d > 0.0) || (!up && d < 0.0) {
                    tp[i] * o.volume[i]
                } else {
                    0.0
                }
            })
            .collect()
    };
    zip(&sum(&flow(true), n), &sum(&flow(false), n), |p, q| 100.0 * ratio(p, p + q, 0.5))
}

/// Elder's force index: EMA of the close change × volume.
pub fn force_index(o: &Ohlcv, n: usize) -> Vec<f64> {
    ema(&zip(&change(&o.close, 1), &o.volume, |d, v| d * v), n)
}

/// Arms' ease of movement: SMA of the median-price move per unit of volume, scaled by
/// `divisor`. Bars without volume contribute nothing.
pub fn ease_of_movement(o: &Ohlcv, n: usize, divisor: f64) -> Vec<f64> {
    let hl2 = o.hl2();
    let raw: Vec<f64> = (0..o.len())
        .map(|i| ratio(divisor * (hl2[i] - ago(&hl2, i, 1)) * (o.high[i] - o.low[i]), o.volume[i], 0.0))
        .collect();
    sma(&raw, n)
}

/// Price volume trend: cumulative volume × percentage close change.
pub fn pvt(o: &Ohlcv) -> Vec<f64> {
    cumulative(o.len(), |i| if i == 0 { 0.0 } else { ratio(step(&o.close, i), o.close[i - 1], 0.0) * o.volume[i] })
}

/// A volume index starting at 1000 that compounds the close's percentage change only on
/// bars where `pick(volume, previous volume)` holds.
fn volume_index(o: &Ohlcv, pick: impl Fn(f64, f64) -> bool) -> Vec<f64> {
    let mut v = 1000.0;
    (0..o.len())
        .map(|i| {
            if !o.volume[i].is_finite() || (i > 0 && !o.volume[i - 1].is_finite()) {
                return NAN;
            }
            if i > 0 && pick(o.volume[i], o.volume[i - 1]) {
                v *= 1.0 + ratio(step(&o.close, i), o.close[i - 1], 0.0);
            }
            v
        })
        .collect()
}

/// Negative volume index (moves on volume declines) with an EMA signal.
pub fn nvi(o: &Ohlcv, signal: usize) -> (Vec<f64>, Vec<f64>) {
    let line = volume_index(o, |v, p| v < p);
    let sig = ema(&line, signal);
    (line, sig)
}

/// Positive volume index (moves on volume increases) with an EMA signal.
pub fn pvi(o: &Ohlcv, signal: usize) -> (Vec<f64>, Vec<f64>) {
    let line = volume_index(o, |v, p| v > p);
    let sig = ema(&line, signal);
    (line, sig)
}

/// Klinger volume oscillator (simplified): EMA difference of volume signed by the
/// typical-price trend, with an EMA signal.
pub fn klinger(o: &Ohlcv, fast: usize, slow: usize, signal: usize) -> Macd {
    let tp = o.hlc3();
    let sv: Vec<f64> = (0..o.len())
        .map(|i| {
            let d = tp[i] - ago(&tp, i, 1);
            if d.is_nan() {
                NAN
            } else if d >= 0.0 {
                o.volume[i]
            } else {
                -o.volume[i]
            }
        })
        .collect();
    let line = zip(&ema(&sv, fast), &ema(&sv, slow), |f, s| f - s);
    let signal = ema(&line, signal);
    let histogram = zip(&line, &signal, |a, b| a - b);
    Macd { macd: line, signal, histogram }
}

/// Volume oscillator: percentage difference of fast and slow volume EMAs.
pub fn volume_osc(o: &Ohlcv, fast: usize, slow: usize) -> Vec<f64> {
    zip(&ema(&o.volume, fast), &ema(&o.volume, slow), |f, s| 100.0 * ratio(f - s, s, 0.0))
}

/// Percentage volume oscillator: PPO of volume, with signal.
pub fn pvo(o: &Ohlcv, fast: usize, slow: usize, signal: usize) -> Macd {
    ppo(&o.volume, fast, slow, signal)
}

/// Volume rate of change in percent over `n` bars.
pub fn vroc(o: &Ohlcv, n: usize) -> Vec<f64> {
    roc(&o.volume, n)
}

/// Relative volume: this bar's volume over the average of the previous `n` bars.
pub fn relative_volume(o: &Ohlcv, n: usize) -> Vec<f64> {
    let avg = sma(&o.volume, n);
    (0..o.len()).map(|i| ratio(o.volume[i], ago(&avg, i, 1), NAN)).collect()
}

/// Twiggs money flow: Wilder-smoothed true-range money flow volume over smoothed volume.
pub fn twiggs_mf(o: &Ohlcv, n: usize) -> Vec<f64> {
    let adv: Vec<f64> = (0..o.len())
        .map(|i| {
            let p = ago(&o.close, i, 1);
            let (th, tl) = (o.high[i].max(p), o.low[i].min(p));
            ratio((o.close[i] - tl) - (th - o.close[i]), th - tl, 0.0) * o.volume[i]
        })
        .collect();
    zip(&rma(&adv, n), &rma(&o.volume, n), |a, v| ratio(a, v, 0.0))
}

/// Williams accumulation/distribution: cumulative close moves measured from the true range
/// extreme on the opposite side.
pub fn williams_ad(o: &Ohlcv) -> Vec<f64> {
    cumulative(o.len(), |i| {
        if i == 0 {
            return 0.0;
        }
        let (p, c) = (o.close[i - 1], o.close[i]);
        if c > p {
            c - o.low[i].min(p)
        } else if c < p {
            c - o.high[i].max(p)
        } else {
            0.0
        }
    })
}

/// Wilder's accumulative swing index with limit move `limit`.
pub fn asi(o: &Ohlcv, limit: f64) -> Vec<f64> {
    cumulative(o.len(), |i| {
        if i == 0 {
            return 0.0;
        }
        let (h, l, c, op) = (o.high[i], o.low[i], o.close[i], o.open[i]);
        let (pc, po) = (o.close[i - 1], o.open[i - 1]);
        let (a, b, cc, d) = ((h - pc).abs(), (l - pc).abs(), h - l, (pc - po).abs());
        let r = if a >= b && a >= cc {
            a - 0.5 * b + 0.25 * d
        } else if b >= a && b >= cc {
            b - 0.5 * a + 0.25 * d
        } else {
            cc + 0.25 * d
        };
        let k = a.max(b);
        ratio(50.0 * (c - pc + 0.5 * (c - op) + 0.25 * (pc - po)) * k, r * limit, 0.0)
    })
}

/// Bill Williams' market facilitation index: range per unit of volume.
pub fn bw_mfi(o: &Ohlcv) -> Vec<f64> {
    (0..o.len()).map(|i| ratio(o.high[i] - o.low[i], o.volume[i], 0.0)).collect()
}

/// Net volume: volume signed by the close's direction.
pub fn net_volume(o: &Ohlcv) -> Vec<f64> {
    (0..o.len())
        .map(|i| {
            let d = step(&o.close, i);
            if !o.volume[i].is_finite() {
                NAN
            } else if d > 0.0 {
                o.volume[i]
            } else if d < 0.0 {
                -o.volume[i]
            } else {
                0.0
            }
        })
        .collect()
}

/// Volume zone oscillator: EMA of direction-signed volume over EMA of volume, −100..100.
pub fn vzo(o: &Ohlcv, n: usize) -> Vec<f64> {
    zip(&ema(&net_volume(o), n), &ema(&o.volume, n), |s, v| 100.0 * ratio(s, v, 0.0))
}

/// Time segmented volume: the `n`-bar sum of close change × volume.
pub fn tsv(o: &Ohlcv, n: usize) -> Vec<f64> {
    sum(&zip(&change(&o.close, 1), &o.volume, |d, v| d * v), n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9 * (1.0 + b.abs())
    }

    fn bars(rows: &[(f64, f64, f64, f64, f64)]) -> Ohlcv {
        let b: Vec<Bar> =
            rows.iter().enumerate().map(|(i, r)| Bar::new(i as i64 * 60, r.0, r.1, r.2, r.3, r.4)).collect();
        Ohlcv::new(&b)
    }

    #[test]
    fn missing_volume_is_unavailable_and_does_not_poison_later_vwap() {
        let raw = [
            Bar::new(0, 10.0, 10.0, 10.0, 10.0, 1.0),
            Bar::new(60, 20.0, 20.0, 20.0, 20.0, f64::NAN),
            Bar::new(120, 30.0, 30.0, 30.0, 30.0, 1.0),
            Bar::new(180, 40.0, 40.0, 40.0, 40.0, 0.0),
            Bar::new(240, 50.0, 50.0, 50.0, 50.0, 1.0),
        ];
        let o = Ohlcv::new(&raw);
        let v = vwap_by(&raw, |_| 0);
        assert!(v[1].is_nan());
        assert_eq!(v[2], 20.0);
        let bands = vwap_bands(&o, |_| 0, 1.0);
        assert!(bands.middle[1].is_nan());
        assert_eq!(bands.middle[2], 20.0);
        let rvol = relative_volume(&o, 2);
        assert!(rvol[1].is_nan() && rvol[2].is_nan() && rvol[3].is_nan());
        assert_eq!(rvol[4], 2.0);
        assert!(super::super::ma::vwma(&o.close, &o.volume, 2)[2].is_nan());
    }

    #[test]
    fn cumulative_flows_hand_computed() {
        let o = bars(&[
            (10.0, 12.0, 8.0, 11.0, 100.0),
            (11.0, 13.0, 10.0, 10.0, 200.0),
            (10.0, 11.0, 9.0, 10.0, 50.0),
            (10.0, 12.0, 10.0, 12.0, 300.0),
        ]);
        assert_eq!(obv(&o), vec![0.0, -200.0, -200.0, 100.0]);
        // Close location: (11−8 − (12−11))/4 = 0.5, (0 − 3)/3 = −1, 0, 1.
        assert_eq!(adl(&o), vec![50.0, -150.0, -150.0, 150.0]);
        assert!(near(cmf(&o, 4)[3], 150.0 / 650.0));
        assert!(near(pvt(&o)[1], -200.0 / 11.0));
        assert_eq!(net_volume(&o), vec![0.0, -200.0, 0.0, 300.0]);
        assert_eq!(williams_ad(&o), vec![0.0, 0.0 + (10.0 - 13.0), -3.0, -3.0 + (12.0 - 10.0)]);
        let (n, _) = nvi(&o, 3);
        let (p, _) = pvi(&o, 3);
        assert!(near(n[2], 1000.0) && near(p[1], 1000.0 * (1.0 - 1.0 / 11.0)));
        assert!(near(n[3], 1000.0) && near(p[3], p[1] * 1.2));
    }

    #[test]
    fn money_flow_index_bounds() {
        let rising: Vec<(f64, f64, f64, f64, f64)> =
            (0..30).map(|i| i as f64 + 10.0).map(|c| (c, c + 1.0, c - 1.0, c, 1000.0)).collect();
        assert_eq!(mfi(&bars(&rising), 14)[29], 100.0);
        let flat: Vec<(f64, f64, f64, f64, f64)> = (0..30).map(|_| (5.0, 5.0, 5.0, 5.0, 0.0)).collect();
        let o = bars(&flat);
        assert_eq!(mfi(&o, 14)[29], 50.0);
        assert_eq!(ease_of_movement(&o, 14, 10_000.0)[29], 0.0);
        assert_eq!(cmf(&o, 20)[29], 0.0);
        assert!(relative_volume(&o, 20)[29].is_nan());
    }

    #[test]
    fn vwap_bands_weight_by_volume() {
        let raw = [Bar::new(0, 0.0, 10.0, 10.0, 10.0, 1.0), Bar::new(60, 0.0, 20.0, 20.0, 20.0, 3.0)];
        let b = vwap_bands(&Ohlcv::new(&raw), |_| 0, 1.0);
        // Mean 17.5; variance (1·56.25 + 3·6.25) / 4 = 18.75.
        assert!(near(b.middle[1], 17.5));
        assert!(near(b.upper[1] - b.middle[1], 18.75f64.sqrt()));
        assert_eq!(b.upper[0], b.lower[0]);
        assert!(near(vwap_by(&raw, |_| 0)[1], b.middle[1]));
    }

    #[test]
    fn oscillators_relate_to_their_parts() {
        let rows: Vec<(f64, f64, f64, f64, f64)> = (0..120)
            .map(|i| {
                let c = 50.0 + (i as f64 * 0.2).sin() * 4.0;
                (c - 0.2, c + 0.6, c - 0.7, c, 1000.0 + (i % 5) as f64 * 150.0)
            })
            .collect();
        let o = bars(&rows);
        let v = &o.volume;
        assert!(near(volume_osc(&o, 5, 10)[60], 100.0 * (ema(v, 5)[60] - ema(v, 10)[60]) / ema(v, 10)[60]));
        assert!(near(relative_volume(&o, 20)[60], v[60] / sma(v, 20)[59]));
        let z = vzo(&o, 14);
        assert!(z.iter().filter(|x| x.is_finite()).all(|x| x.abs() <= 100.0));
        let k = klinger(&o, 34, 55, 13);
        assert!(near(k.histogram[100], k.macd[100] - k.signal[100]));
        assert!(twiggs_mf(&o, 21)[100].abs() <= 1.0);
        assert!(asi(&o, 10_000.0)[119].is_finite());
        assert!(near(tsv(&o, 13)[50], (38..=50).map(|i| (o.close[i] - o.close[i - 1]) * v[i]).sum::<f64>()));
    }
}
