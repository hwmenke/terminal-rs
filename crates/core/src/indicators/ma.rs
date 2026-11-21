//! Moving averages and adaptive filters, including John Ehlers' DSP smoothers.

use std::f64::consts::PI;

use super::stats::{change, highest, lowest, sum, windows};
use super::{NAN, Ohlcv, from_first, ratio, zip};

/// Simple moving average over `n` values.
pub fn sma(values: &[f64], n: usize) -> Vec<f64> {
    let k = n as f64;
    sum(values, n).into_iter().map(|s| s / k).collect()
}

/// Exponential smoothing with factor `alpha`, seeded with the SMA of the first `n` finite
/// values. A non-finite input yields NaN at that bar and leaves the state untouched.
pub(super) fn smooth(values: &[f64], n: usize, alpha: f64) -> Vec<f64> {
    let mut out = vec![NAN; values.len()];
    if n == 0 {
        return out;
    }
    let (mut seen, mut acc) = (0usize, 0.0);
    for (i, &v) in values.iter().enumerate() {
        if !v.is_finite() {
            continue;
        }
        if seen < n {
            acc += v;
            seen += 1;
            if seen < n {
                continue;
            }
            acc /= n as f64;
        } else {
            acc = alpha * v + (1.0 - alpha) * acc;
        }
        out[i] = acc;
    }
    out
}

/// Exponential moving average, alpha = 2/(n+1).
pub fn ema(values: &[f64], n: usize) -> Vec<f64> {
    smooth(values, n, 2.0 / (n as f64 + 1.0))
}

/// Wilder's moving average (RMA, SMMA), alpha = 1/n.
pub fn rma(values: &[f64], n: usize) -> Vec<f64> {
    smooth(values, n, 1.0 / n as f64)
}

/// Linearly weighted moving average (newest weight n, oldest 1), O(1) per bar.
pub fn wma(values: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![NAN; values.len()];
    let denom = (n * (n + 1)) as f64 / 2.0;
    windows(values, n, |i, r, _, w, _| out[i] = w / denom + r);
    out
}

/// Hull moving average: WMA(2·WMA(n/2) − WMA(n), √n).
pub fn hma(values: &[f64], n: usize) -> Vec<f64> {
    if n < 2 {
        return wma(values, n);
    }
    let diff = zip(&wma(values, n / 2), &wma(values, n), |a, b| 2.0 * a - b);
    wma(&diff, ((n as f64).sqrt() as usize).max(1))
}

/// Double EMA: 2·EMA − EMA(EMA).
pub fn dema(values: &[f64], n: usize) -> Vec<f64> {
    let e1 = ema(values, n);
    let e2 = ema(&e1, n);
    zip(&e1, &e2, |a, b| 2.0 * a - b)
}

/// Triple EMA: 3·E1 − 3·E2 + E3.
pub fn tema(values: &[f64], n: usize) -> Vec<f64> {
    let e1 = ema(values, n);
    let e2 = ema(&e1, n);
    let e3 = ema(&e2, n);
    (0..values.len()).map(|i| 3.0 * e1[i] - 3.0 * e2[i] + e3[i]).collect()
}

/// Triangular moving average: an SMA of an SMA spanning `n` bars in total.
pub fn trima(values: &[f64], n: usize) -> Vec<f64> {
    if n == 0 {
        return vec![NAN; values.len()];
    }
    let (a, b) = if n % 2 == 1 { (n.div_ceil(2), n.div_ceil(2)) } else { (n / 2, n / 2 + 1) };
    sma(&sma(values, a), b)
}

/// Kaufman adaptive moving average: the smoothing constant moves between the `fast` and
/// `slow` EMA constants with the efficiency ratio over `n` bars.
pub fn kama(values: &[f64], n: usize, fast: usize, slow: usize) -> Vec<f64> {
    from_first(values, |v| {
        let mut out = vec![NAN; v.len()];
        if n == 0 || v.len() < n {
            return out;
        }
        let (fsc, ssc) = (2.0 / (fast as f64 + 1.0), 2.0 / (slow as f64 + 1.0));
        let mut k = v[n - 1];
        out[n - 1] = k;
        let mut vol: f64 = (1..n.min(v.len())).map(|j| (v[j] - v[j - 1]).abs()).sum();
        for i in n..v.len() {
            vol += (v[i] - v[i - 1]).abs();
            if i > n {
                vol -= (v[i - n] - v[i - n - 1]).abs();
            }
            let er = ratio((v[i] - v[i - n]).abs(), vol, 0.0).clamp(0.0, 1.0);
            let sc = (er * (fsc - ssc) + ssc).powi(2);
            k += sc * (v[i] - k);
            out[i] = k;
        }
        out
    })
}

/// Zero-lag EMA: an EMA of `2·x − x[lag]` with lag (n−1)/2.
pub fn zlema(values: &[f64], n: usize) -> Vec<f64> {
    let lag = n.saturating_sub(1) / 2;
    let adj: Vec<f64> =
        (0..values.len()).map(|i| if i >= lag { 2.0 * values[i] - values[i - lag] } else { NAN }).collect();
    ema(&adj, n)
}

/// Arnaud Legoux moving average: Gaussian weights centred at `offset` (0..1) across the
/// window, width n/`sigma`.
pub fn alma(values: &[f64], n: usize, offset: f64, sigma: f64) -> Vec<f64> {
    let mut out = vec![NAN; values.len()];
    if n == 0 || sigma <= 0.0 {
        return out;
    }
    let m = offset * (n - 1) as f64;
    let s = n as f64 / sigma;
    let w: Vec<f64> = (0..n).map(|j| (-(j as f64 - m).powi(2) / (2.0 * s * s)).exp()).collect();
    let norm: f64 = w.iter().sum();
    for i in n - 1..values.len() {
        let win = &values[i + 1 - n..=i];
        if win.iter().all(|x| x.is_finite()) {
            out[i] = win.iter().zip(&w).map(|(x, w)| x * w).sum::<f64>() / norm;
        }
    }
    out
}

/// Volume-weighted moving average (falls back to the SMA where the window has no volume).
pub fn vwma(values: &[f64], volume: &[f64], n: usize) -> Vec<f64> {
    let pv = zip(values, volume, |c, v| c * v);
    let (spv, sv, sc) = (sum(&pv, n), sum(volume, n), sma(values, n));
    (0..values.len())
        .map(|i| {
            if !sv[i].is_finite() {
                NAN
            } else if sv[i] > 0.0 {
                spv[i] / sv[i]
            } else {
                sc[i]
            }
        })
        .collect()
}

/// Elastic volume-weighted moving average: each bar moves the average by its share of the
/// last `n` bars' volume.
pub fn evwma(values: &[f64], volume: &[f64], n: usize) -> Vec<f64> {
    let sv = sum(volume, n);
    let mut out = vec![NAN; values.len()];
    let mut prev = NAN;
    for i in 0..values.len() {
        if !sv[i].is_finite() || !values[i].is_finite() {
            continue;
        }
        prev = if prev.is_nan() {
            values[i]
        } else if sv[i] > 0.0 {
            ((sv[i] - volume[i]) * prev + volume[i] * values[i]) / sv[i]
        } else {
            prev
        };
        out[i] = prev;
    }
    out
}

/// Tillson T3: six chained EMAs combined with volume factor `v`.
pub fn t3(values: &[f64], n: usize, v: f64) -> Vec<f64> {
    let e1 = ema(values, n);
    let e2 = ema(&e1, n);
    let e3 = ema(&e2, n);
    let e4 = ema(&e3, n);
    let e5 = ema(&e4, n);
    let e6 = ema(&e5, n);
    let (v2, v3) = (v * v, v * v * v);
    let (c1, c2, c3, c4) = (-v3, 3.0 * v2 + 3.0 * v3, -6.0 * v2 - 3.0 * v - 3.0 * v3, 1.0 + 3.0 * v + v3 + 3.0 * v2);
    (0..values.len()).map(|i| c1 * e6[i] + c2 * e5[i] + c3 * e4[i] + c4 * e3[i]).collect()
}

/// McGinley Dynamic: an EMA-seeded average whose speed adapts to (price/average)^4. The
/// step is capped at the full distance so it never overshoots.
pub fn mcginley(values: &[f64], n: usize) -> Vec<f64> {
    let seed = ema(values, n);
    let mut out = vec![NAN; values.len()];
    let mut md = NAN;
    for i in 0..values.len() {
        let x = values[i];
        if md.is_nan() {
            md = seed[i];
        } else if x.is_finite() {
            let d = n as f64 * (x / md).powi(4);
            md = if md != 0.0 && d.is_finite() { md + (x - md) / d.max(1.0) } else { x };
        }
        out[i] = md;
    }
    out
}

/// Chande's variable index dynamic average: an EMA whose factor scales with |CMO(`cmo_n`)|.
pub fn vidya(values: &[f64], n: usize, cmo_n: usize) -> Vec<f64> {
    let ch = change(values, 1);
    let up: Vec<f64> = ch.iter().map(|c| if c.is_nan() { NAN } else { c.max(0.0) }).collect();
    let dn: Vec<f64> = ch.iter().map(|c| if c.is_nan() { NAN } else { (-c).max(0.0) }).collect();
    let (su, sd) = (sum(&up, cmo_n), sum(&dn, cmo_n));
    let alpha = 2.0 / (n as f64 + 1.0);
    let mut out = vec![NAN; values.len()];
    let mut v = NAN;
    for i in 0..values.len() {
        if !su[i].is_finite() || !sd[i].is_finite() || !values[i].is_finite() {
            continue;
        }
        let k = alpha * ratio(su[i] - sd[i], su[i] + sd[i], 0.0).abs();
        v = if v.is_nan() { values[i] } else { k * values[i] + (1.0 - k) * v };
        out[i] = v;
    }
    out
}

/// Ehlers' fractal adaptive moving average of the median price over `n` (even) bars.
pub fn frama(d: &Ohlcv, n: usize) -> Vec<f64> {
    let n = n.max(2).next_multiple_of(2);
    let h = n / 2;
    let price = d.hl2();
    let (hh1, ll1) = (highest(&d.high, h), lowest(&d.low, h));
    let (hh, ll) = (highest(&d.high, n), lowest(&d.low, n));
    let mut out = vec![NAN; d.len()];
    let (mut dim, mut filt) = (1.0, NAN);
    for i in n - 1..d.len() {
        let n1 = (hh1[i] - ll1[i]) / h as f64;
        let n2 = (hh1[i - h] - ll1[i - h]) / h as f64;
        let n3 = (hh[i] - ll[i]) / n as f64;
        if !(n1.is_finite() && n2.is_finite() && n3.is_finite() && price[i].is_finite()) {
            continue;
        }
        if n1 > 0.0 && n2 > 0.0 && n3 > 0.0 {
            dim = ((n1 + n2).ln() - n3.ln()) / 2f64.ln();
        }
        let alpha = (-4.6 * (dim - 1.0)).exp().clamp(0.01, 1.0);
        filt = if filt.is_nan() { price[i] } else { alpha * price[i] + (1.0 - alpha) * filt };
        out[i] = filt;
    }
    out
}

/// Ehlers' two-pole super smoother with critical period `n`.
pub fn super_smoother(values: &[f64], n: usize) -> Vec<f64> {
    if n == 0 {
        return vec![NAN; values.len()];
    }
    from_first(values, |v| {
        let a1 = (-1.414 * PI / n as f64).exp();
        let c2 = 2.0 * a1 * (1.414 * PI / n as f64).cos();
        let c3 = -a1 * a1;
        let c1 = 1.0 - c2 - c3;
        let mut out = v.to_vec();
        for i in 2..v.len() {
            out[i] = c1 * (v[i] + v[i - 1]) / 2.0 + c2 * out[i - 1] + c3 * out[i - 2];
        }
        out
    })
}

/// Ehlers' simple decycler: the series minus its two-pole high-pass component, which
/// removes cycles shorter than `n` bars with no lag. Cutoffs under 3 bars put the
/// high-pass pole outside the unit circle, so they are blank.
pub fn decycler(values: &[f64], n: usize) -> Vec<f64> {
    if n < 3 {
        return vec![NAN; values.len()];
    }
    from_first(values, |v| {
        let w = 0.707 * 2.0 * PI / n as f64;
        let a = (w.cos() + w.sin() - 1.0) / w.cos();
        let (k1, k2, k3) = ((1.0 - a / 2.0).powi(2), 2.0 * (1.0 - a), (1.0 - a).powi(2));
        let mut hp = vec![0.0; v.len()];
        for i in 2..v.len() {
            hp[i] = k1 * (v[i] - 2.0 * v[i - 1] + v[i - 2]) + k2 * hp[i - 1] - k3 * hp[i - 2];
        }
        zip(v, &hp, |x, h| x - h)
    })
}

fn at(v: &[f64], i: usize, k: usize) -> f64 {
    if i >= k { v[i - k] } else { 0.0 }
}

/// MESA adaptive moving average and its following average.
#[derive(Debug, Clone)]
pub struct Mama {
    pub mama: Vec<f64>,
    pub fama: Vec<f64>,
}

/// Ehlers' MESA adaptive moving average: a Hilbert-transform phase rate sets the EMA
/// factor between `slow` and `fast` limits.
pub fn mama(values: &[f64], fast: f64, slow: f64) -> Mama {
    let len = values.len();
    let mut out = Mama { mama: vec![NAN; len], fama: vec![NAN; len] };
    let Some(start) = values.iter().position(|v| v.is_finite()) else { return out };
    let p = &values[start..];
    let m = p.len();
    let [mut smooth, mut detr, mut i1, mut q1, mut i2, mut q2, mut re, mut im, mut period, mut phase] =
        std::array::from_fn(|_| vec![0.0; m]);
    let (mut ma, mut fa) = (p[0], p[0]);
    for i in 0..m {
        let pr = |k: usize| if i >= k { p[i - k] } else { p[0] };
        smooth[i] = (4.0 * pr(0) + 3.0 * pr(1) + 2.0 * pr(2) + pr(3)) / 10.0;
        let adj = 0.075 * at(&period, i, 1) + 0.54;
        let hilbert =
            |v: &[f64]| (0.0962 * v[i] + 0.5769 * at(v, i, 2) - 0.5769 * at(v, i, 4) - 0.0962 * at(v, i, 6)) * adj;
        detr[i] = hilbert(&smooth);
        q1[i] = hilbert(&detr);
        i1[i] = at(&detr, i, 3);
        let (ji, jq) = (hilbert(&i1), hilbert(&q1));
        i2[i] = 0.2 * (i1[i] - jq) + 0.8 * at(&i2, i, 1);
        q2[i] = 0.2 * (q1[i] + ji) + 0.8 * at(&q2, i, 1);
        let r = i2[i] * at(&i2, i, 1) + q2[i] * at(&q2, i, 1);
        let j = i2[i] * at(&q2, i, 1) - q2[i] * at(&i2, i, 1);
        re[i] = 0.2 * r + 0.8 * at(&re, i, 1);
        im[i] = 0.2 * j + 0.8 * at(&im, i, 1);
        let prev = at(&period, i, 1);
        let mut per = prev;
        if im[i] != 0.0 && re[i] != 0.0 {
            per = 2.0 * PI / (im[i] / re[i]).atan();
        }
        per = per.min(1.5 * prev).max(0.67 * prev).clamp(6.0, 50.0);
        period[i] = 0.2 * per + 0.8 * prev;
        phase[i] = if i1[i] != 0.0 { (q1[i] / i1[i]).atan().to_degrees() } else { at(&phase, i, 1) };
        let delta = (at(&phase, i, 1) - phase[i]).max(1.0);
        let alpha = (fast / delta).max(slow);
        ma = alpha * p[i] + (1.0 - alpha) * ma;
        fa = 0.5 * alpha * ma + (1.0 - 0.5 * alpha) * fa;
        if i >= 6 {
            out.mama[start + i] = ma;
            out.fama[start + i] = fa;
        }
    }
    out
}

/// Ehlers' instantaneous trendline and its trigger (2·IT − IT[2]).
pub fn itrend(values: &[f64], alpha: f64) -> (Vec<f64>, Vec<f64>) {
    let it = from_first(values, |p| {
        let mut it = vec![0.0; p.len()];
        let a = alpha;
        for i in 0..p.len() {
            let pr = |k: usize| if i >= k { p[i - k] } else { p[0] };
            it[i] = if i < 7 {
                (pr(0) + 2.0 * pr(1) + pr(2)) / 4.0
            } else {
                (a - a * a / 4.0) * p[i] + 0.5 * a * a * p[i - 1] - (a - 0.75 * a * a) * p[i - 2]
                    + 2.0 * (1.0 - a) * it[i - 1]
                    - (1.0 - a).powi(2) * it[i - 2]
            };
        }
        it
    });
    let trigger = (0..it.len()).map(|i| if i >= 2 { 2.0 * it[i] - it[i - 2] } else { NAN }).collect();
    (it, trigger)
}

/// Symmetrically weighted moving average over 4 bars (weights 1, 2, 2, 1).
pub(super) fn swma(values: &[f64]) -> Vec<f64> {
    (0..values.len())
        .map(
            |i| {
                if i < 3 { NAN } else { (values[i] + 2.0 * values[i - 1] + 2.0 * values[i - 2] + values[i - 3]) / 6.0 }
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9 * (1.0 + b.abs())
    }

    fn wave(n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| 100.0 + (i as f64 * 0.21).sin() * 8.0 + (i as f64 * 0.05).cos() * 3.0 + i as f64 * 0.04)
            .collect()
    }

    #[test]
    fn wma_hand_computed_and_rolling() {
        // (1·1 + 2·2 + 3·3) / 6 = 14/6; (2 + 6 + 12) / 6 = 20/6.
        let w = wma(&[1.0, 2.0, 3.0, 4.0], 3);
        assert!(w[1].is_nan() && near(w[2], 14.0 / 6.0) && near(w[3], 20.0 / 6.0));
        let xs = wave(300);
        let got = wma(&xs, 9);
        for i in 8..300 {
            let want = (0..9).map(|k| (k + 1) as f64 * xs[i - 8 + k]).sum::<f64>() / 45.0;
            assert!(near(got[i], want), "{i}");
        }
    }

    #[test]
    fn compositions_follow_their_definitions() {
        let xs = wave(400);
        let (e1, e2) = (ema(&xs, 10), ema(&ema(&xs, 10), 10));
        let d = dema(&xs, 10);
        assert!(near(d[300], 2.0 * e1[300] - e2[300]));
        let h = hma(&xs, 16);
        let diff = zip(&wma(&xs, 8), &wma(&xs, 16), |a, b| 2.0 * a - b);
        assert!(near(h[300], wma(&diff, 4)[300]));
        // TRIMA spans n bars: its first value is at n - 1.
        let t = trima(&xs, 10);
        assert!(t[8].is_nan() && t[9].is_finite());
        let t = trima(&xs, 9);
        assert!(t[7].is_nan() && t[8].is_finite());
        // T3 coefficients sum to 1, so a constant stays constant.
        assert!(near(t3(&[5.0; 200], 5, 0.7)[199], 5.0));
    }

    #[test]
    fn adaptive_averages_track_a_constant_and_a_trend() {
        let flat = vec![42.0; 300];
        for v in [
            kama(&flat, 10, 2, 30),
            zlema(&flat, 20),
            alma(&flat, 9, 0.85, 6.0),
            mcginley(&flat, 14),
            vidya(&flat, 20, 9),
            super_smoother(&flat, 10),
            decycler(&flat, 60),
            mama(&flat, 0.5, 0.05).mama,
            itrend(&flat, 0.07).0,
        ] {
            assert!(near(v[299], 42.0), "{}", v[299]);
        }
        // With a perfectly efficient trend KAMA uses the fast constant: k += (2/3)^2 (x - k).
        let line: Vec<f64> = (0..50).map(|i| i as f64).collect();
        let k = kama(&line, 10, 2, 30);
        let fsc: f64 = (2.0f64 / 3.0).powi(2);
        assert!(near(k[10], 9.0 + fsc * (10.0 - 9.0)));
        // ALMA weights sum to 1 and lean to the recent end with offset 0.85.
        let a = alma(&line, 9, 0.85, 6.0);
        assert!(a[20] > sma(&line, 9)[20] && a[20] < 20.0);
    }

    #[test]
    fn volume_weighting() {
        let c = [10.0, 20.0, 30.0];
        let v = [1.0, 1.0, 2.0];
        assert!(near(vwma(&c, &v, 3)[2], (10.0 + 20.0 + 60.0) / 4.0));
        assert!(near(vwma(&c, &[0.0; 3], 3)[2], 20.0), "no volume falls back to the SMA");
        let e = evwma(&c, &v, 2);
        assert!(e[0].is_nan() && near(e[1], 20.0));
        assert!(near(e[2], ((3.0 - 2.0) * 20.0 + 2.0 * 30.0) / 3.0));
    }

    #[test]
    fn frama_moves_between_bounds() {
        let bars: Vec<crate::Bar> = wave(200)
            .into_iter()
            .enumerate()
            .map(|(i, c)| crate::Bar::new(i as i64, c, c + 1.0, c - 1.0, c, 1.0))
            .collect();
        let f = frama(&Ohlcv::new(&bars), 16);
        assert!(f[14].is_nan() && f[15].is_finite());
        // A running blend of prices never leaves the range seen so far.
        let price: Vec<f64> = bars.iter().map(|b| (b.high + b.low) / 2.0).collect();
        for i in 15..f.len() {
            let seen = &price[15..=i];
            let (lo, hi) = seen.iter().fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
            assert!(f[i] >= lo - 1e-9 && f[i] <= hi + 1e-9, "frama[{i}] = {} outside {lo}..{hi}", f[i]);
        }
    }

    #[test]
    fn decycler_is_stable_from_its_shortest_cutoff() {
        let xs: Vec<f64> = (0..2000).map(|i| 100.0 + (i as f64 * 1.7).sin()).collect();
        for n in 3..=10 {
            let d = decycler(&xs, n);
            assert!(d.iter().all(|v| (90.0..110.0).contains(v)), "cutoff {n} runs away");
        }
        for n in 0..3 {
            assert!(decycler(&xs, n).iter().all(|v| v.is_nan()), "cutoff {n} is unstable");
        }
    }
}
