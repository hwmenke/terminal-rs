//! Closed-bar ZBAND diagnostics. No fetching, filling or simulated data.
use tr_core::{
    AssetClass, Bar, Timeframe,
    indicators::registry::{ZBand, zband},
};

pub const HORIZONS: [usize; 3] = [2, 5, 10];
pub const DIAGNOSTIC_BARS: usize = 512;
/// Two-sided Bonferroni 5% family threshold for the three fixed horizons.
pub const CRITICAL_Z: f64 = 2.394;
pub const SIGMA: &str = "rolling price sample σ (n−1)";
pub const GAUSSIAN_TAILS: [f64; 4] = [0.1586552539, 0.1586552539, 0.02275013195, 0.02275013195];

#[derive(Debug, Clone, PartialEq)]
pub struct VarianceRatio {
    pub horizon: usize,
    pub returns: usize,
    pub ratio: Option<f64>,
    pub z_star: Option<f64>,
}
/// Lo–MacKinlay overlapping bias-adjusted VR and heteroskedastic robust z*.
/// This consumes log returns, not price-level changes. At least 20q returns.
pub fn variance_ratio(returns: &[f64], q: usize) -> VarianceRatio {
    let n = returns.len();
    let mut out = VarianceRatio { horizon: q, returns: n, ratio: None, z_star: None };
    if q < 2 || n < q.saturating_mul(20) || returns.iter().any(|r| !r.is_finite()) {
        return out;
    }
    let mean = returns.iter().sum::<f64>() / n as f64;
    let residuals: Vec<_> = returns.iter().map(|r| r - mean).collect();
    let ss: f64 = residuals.iter().map(|r| r * r).sum();
    if ss <= n as f64 * (64.0 * f64::EPSILON).powi(2) {
        return out;
    }
    let sigma_one = ss / (n - 1) as f64;
    let m = q as f64 * (n - q + 1) as f64 * (1.0 - q as f64 / n as f64);
    let sigma_q = residuals.windows(q).map(|w| w.iter().sum::<f64>().powi(2)).sum::<f64>() / m;
    let vr = sigma_q / sigma_one;
    // delta_j = N sum(e_t² e_(t-j)²)/(sum e_t²)²;
    // theta = sum [2(q-j)/q]² delta_j; z* = sqrt(N)(VR-1)/sqrt(theta).
    // Cancel N analytically to avoid an extra factor of sqrt(N).
    let variance: f64 = (1..q)
        .map(|j| {
            let product = residuals[j..].iter().zip(&residuals[..n - j]).map(|(a, b)| (a * a) * (b * b)).sum::<f64>();
            (2.0 * (q - j) as f64 / q as f64).powi(2) * product / (ss * ss)
        })
        .sum();
    out.ratio = vr.is_finite().then_some(vr);
    out.z_star = (variance > 0.0).then(|| (vr - 1.0) / variance.sqrt()).filter(|z| z.is_finite());
    out
}

/// Order: above +1, below −1, above +2, below −2. Strict exceedances, finite z only.
pub fn tails(z: &[f64]) -> (usize, [usize; 4]) {
    let mut counts = [0; 4];
    let mut n = 0;
    for &z in z.iter().filter(|z| z.is_finite()) {
        n += 1;
        for (count, hit) in counts.iter_mut().zip([z > 1.0, z < -1.0, z > 2.0, z < -2.0]) {
            *count += usize::from(hit);
        }
    }
    (n, counts)
}
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub closed: usize,
    pub first_ts: i64,
    pub last_ts: i64,
    pub vr: [VarianceRatio; 3],
    pub tail_n: usize,
    pub tail_counts: [usize; 4],
    pub trending: bool,
    pub label_text: String,
    pub tail_text: String,
    pub detail_text: String,
}
impl Report {
    pub fn available(&self) -> bool {
        self.vr.iter().all(|v| v.z_star.is_some())
    }
    pub fn muted(&self) -> bool {
        self.trending || !self.available()
    }
    pub fn verdict(&self) -> &'static str {
        if self.trending {
            "TREND PERSISTENCE · bands grey"
        } else if !self.available() {
            "VR unavailable · bands grey"
        } else if self.vr.iter().any(|v| v.z_star.is_some_and(|z| z < -CRITICAL_Z)) {
            "ANTI-PERSISTENCE · not proof of level mean reversion"
        } else {
            "RANDOM WALK NOT REJECTED · no mean-reversion claim"
        }
    }
    pub fn label(&self) -> String {
        let rates: Vec<_> = self
            .vr
            .iter()
            .map(|v| match (v.ratio, v.z_star) {
                (Some(vr), Some(z)) => format!("q{} {vr:.2}/{z:+.2}", v.horizon),
                _ => format!("q{} —", v.horizon),
            })
            .collect();
        format!("{SIGMA} · {} · VR/z* {}", self.verdict(), rates.join(" · "))
    }
    pub fn detail(&self) -> String {
        let mut out = format!(
            "CURRENT CLOSED SNAPSHOT · {} … {} UTC · {} closed bars\n{}\nVR/z*: q2,5,10 on up to 512 closes; ≥20q log returns each. Bonferroni family 5%, |z*|>{CRITICAL_Z}. Failure to reject a random walk is not mean reversion.\nEmpirical band tails: rolling in-window price z (includes the observation); descriptive, overlapping and not independent. n={} finite z.\n",
            stamp(self.first_ts),
            stamp(self.last_ts),
            self.closed,
            self.label(),
            self.tail_n
        );
        for ((name, count), expected) in
            ["above +1σ", "below −1σ", "above +2σ", "below −2σ"].iter().zip(self.tail_counts).zip(GAUSSIAN_TAILS)
        {
            let rate = if self.tail_n > 0 {
                format!("{:.2}%", 100.0 * count as f64 / self.tail_n as f64)
            } else {
                "—".into()
            };
            out.push_str(&format!("{name}: {count}/{} = {rate} · Gaussian {:.2}%\n", self.tail_n, 100.0 * expected));
        }
        out.push_str("Grey styling uses this latest closed snapshot across the visible history; it is not a causal historical signal. Sigma is price-level sample standard deviation, not return volatility. Forming bars excluded.");
        out
    }
}
fn stamp(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ts.to_string())
}

/// Call on a worker after witnessed vault provenance normalization. Admission
/// checks every bar before excluding a forming tail. Gaps are never bridged.
pub fn compute(
    bars: &[Bar],
    tf: Timeframe,
    class: AssetClass,
    observed: Option<i64>,
    window: usize,
    gaps: &[(i64, i64)],
) -> Result<(ZBand, Report), String> {
    crate::stats::require_research_bars(bars, "Honest Z")?;
    let closed = crate::closed::closed_len(tf, bars, class, observed);
    if window < 2 || closed < window {
        return Err(format!("ZBAND needs at least {} verified closed bars", window.max(2)));
    }
    if gaps.iter().any(|g| g.1 > bars[0].ts && g.0 < bars[closed - 1].ts) {
        return Err("Honest Z refused: UNVERIFIED JOIN in the loaded history".into());
    }
    let closes: Vec<_> = bars[..closed].iter().map(|b| b.close).collect();
    let mut band = zband(&closes, window);
    let start = closed.saturating_sub(DIAGNOSTIC_BARS);
    let returns: Vec<_> = closes[start..].windows(2).map(|w| w[1].ln() - w[0].ln()).collect();
    let vr = HORIZONS.map(|q| variance_ratio(&returns, q));
    let (tail_n, tail_counts) = tails(&band.z[start..]);
    let trending = vr.iter().any(|v| v.ratio.is_some_and(|r| r > 1.0) && v.z_star.is_some_and(|z| z > CRITICAL_Z));
    let mut report = Report {
        closed,
        first_ts: bars[start].ts,
        last_ts: bars[closed - 1].ts,
        vr,
        tail_n,
        tail_counts,
        trending,
        label_text: String::new(),
        tail_text: String::new(),
        detail_text: String::new(),
    };
    report.label_text = report.label();
    report.detail_text = report.detail();
    report.tail_text = if tail_n > 0 {
        let rates: Vec<_> = ["+1", "−1", "+2", "−2"]
            .iter()
            .zip(tail_counts)
            .zip(GAUSSIAN_TAILS)
            .map(|((name, n), g)| format!("{name}σ {:.1}%/{:.1}%", 100.0 * n as f64 / tail_n as f64, 100.0 * g))
            .collect();
        format!("TAILS empirical/Gaussian · {} · n={tail_n}", rates.join(" · "))
    } else {
        "TAILS unavailable · zero finite z observations".into()
    };
    // Preserve chart index alignment; no forming band geometry or z readout.
    for values in [
        &mut band.mean,
        &mut band.sigma,
        &mut band.z,
        &mut band.upper_one,
        &mut band.lower_one,
        &mut band.upper_two,
        &mut band.lower_two,
    ] {
        values.resize(bars.len(), f64::NAN);
    }
    Ok((band, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(returns: &[f64]) -> Vec<Bar> {
        let mut log = 4.0;
        std::iter::once(0.0)
            .chain(returns.iter().copied())
            .enumerate()
            .map(|(i, r)| {
                log += r;
                let c = log.exp();
                Bar::new(i as i64 * 86400, c, c, c, c, 1.0).with_provenance(tr_core::Provenance::Real)
            })
            .collect()
    }
    fn series(n: usize, phi: f64) -> Vec<f64> {
        let mut rng = crate::nulls::SplitMix64(948123);
        let mut prev = 0.0;
        (0..n)
            .map(|_| {
                let noise = ((rng.next() >> 11) as f64 / (1_u64 << 53) as f64 - 0.5) * 0.01;
                prev = phi * prev + noise;
                prev
            })
            .collect()
    }
    #[test]
    fn vr_known_alternating_vector_has_correct_robust_scaling() {
        // 200 mean-zero returns ±.01; every two-step sum=0 -> VR=0.
        // delta_1 (with N cancelled)=199/200² -> z*=−200/sqrt(199).
        let r: Vec<_> = (0..200).map(|i| if i % 2 == 0 { 0.01 } else { -0.01 }).collect();
        let v = variance_ratio(&r, 2);
        assert_eq!(v.ratio, Some(0.0));
        assert!((v.z_star.unwrap() + 200.0 / 199.0_f64.sqrt()).abs() < 1e-10);
    }
    #[test]
    fn random_walk_and_ar1_match_population_variance_ratios() {
        let rw = series(20000, 0.0);
        let ar = series(20000, 0.6);
        for q in HORIZONS {
            let v = variance_ratio(&rw, q);
            assert!((v.ratio.unwrap() - 1.0).abs() < 0.08);
            assert!(v.z_star.unwrap().abs() < CRITICAL_Z);
            let expected =
                1.0 + 2.0 * (1..q).map(|j| (1.0 - j as f64 / q as f64) * 0.6_f64.powi(j as i32)).sum::<f64>();
            let v = variance_ratio(&ar, q);
            assert!((v.ratio.unwrap() - expected).abs() < 0.2, "q{q}: {v:?} != {expected}");
            assert!(v.z_star.unwrap() > CRITICAL_Z);
        }
    }
    #[test]
    fn tails_count_each_band_strictly_and_exclude_nonfinite() {
        assert_eq!(tails(&[-3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0, f64::NAN, f64::INFINITY]), (7, [2, 2, 1, 1]));
        assert_eq!(tails(&[f64::NAN]), (0, [0; 4]));
    }
    #[test]
    fn zero_variance_and_short_windows_are_unavailable() {
        for r in [vec![0.0; 512], vec![0.01; 512], vec![0.01; 20]] {
            let v = variance_ratio(&r, 10);
            assert!(v.ratio.is_none() && v.z_star.is_none());
        }
        let b = fixture(&[0.0; 300]);
        let (_, r) = compute(&b, Timeframe::D1, AssetClass::Crypto, None, 20, &[]).unwrap();
        assert!(r.muted() && !r.trending);
        assert_eq!(r.tail_n, 0);
    }
    #[test]
    fn forming_mutation_cannot_change_diagnostics_or_closed_bands() {
        let mut b = fixture(&series(600, 0.7));
        let (a, r) = compute(&b, Timeframe::D1, AssetClass::Crypto, None, 20, &[]).unwrap();
        let last = b.len() - 1;
        b[last].close *= 1.1;
        b[last].high = b[last].close;
        let (c, s) = compute(&b, Timeframe::D1, AssetClass::Crypto, None, 20, &[]).unwrap();
        assert_eq!(r, s);
        assert!(r.trending);
        assert_eq!(&a.z[20..last], &c.z[20..last]);
        assert!(a.z[last].is_nan() && a.upper_one[last].is_nan());
        b[last].provenance = tr_core::Provenance::Sample;
        assert!(compute(&b, Timeframe::D1, AssetClass::Crypto, None, 20, &[]).is_err());
    }
    #[test]
    fn unverified_short_and_gapped_inputs_are_refused() {
        let mut b = fixture(&series(300, 0.0));
        assert!(compute(&b[..10], Timeframe::D1, AssetClass::Crypto, None, 20, &[]).is_err());
        assert!(compute(&b, Timeframe::D1, AssetClass::Crypto, None, 20, &[(b[10].ts, b[11].ts)]).is_err());
        b[0].provenance = tr_core::Provenance::Missing;
        assert!(compute(&b, Timeframe::D1, AssetClass::Crypto, None, 20, &[]).is_err());
    }
}
