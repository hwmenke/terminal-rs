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
impl VarianceRatio {
    /// Two-sided normal p value of the heteroskedasticity-robust z*.
    pub fn p_two_sided(&self) -> Option<f64> {
        self.z_star.map(|z| erfc(z.abs() / std::f64::consts::SQRT_2).min(1.0))
    }
}

/// Lo–MacKinlay overlapping bias-adjusted VR and heteroskedastic robust z*.
/// This consumes log returns, not price-level changes. At least 20q returns.
pub fn variance_ratio(returns: &[f64], q: usize) -> VarianceRatio {
    let mut core = VrCore::default();
    match core.prepare(returns) {
        true => core.ratio(q),
        false => VarianceRatio { horizon: q, returns: returns.len(), ratio: None, z_star: None },
    }
}

/// Homoskedastic asymptotic z(q) = √N (VR−1) / √(2(2q−1)(q−1)/(3q)) (Lo–MacKinlay
/// eq. 14, overlapping estimator). Reported for comparison; verdicts use robust z*.
pub fn homoskedastic_z(v: &VarianceRatio) -> Option<f64> {
    let q = v.horizon as f64;
    let phi = 2.0 * (2.0 * q - 1.0) * (q - 1.0) / (3.0 * q);
    v.ratio.filter(|_| phi > 0.0).map(|vr| (v.returns as f64).sqrt() * (vr - 1.0) / phi.sqrt())
}

/// Shared residuals and δ_j numerators so one window serves every horizon.
#[derive(Default)]
struct VrCore {
    residuals: Vec<f64>,
    ss: f64,
    /// Σ e_t² e_(t−j)² for j = 1..max(HORIZONS)−1 (index j−1).
    products: Vec<f64>,
}
impl VrCore {
    /// False when the window is too short for the smallest horizon, non-finite or degenerate.
    fn prepare(&mut self, returns: &[f64]) -> bool {
        let n = returns.len();
        self.residuals.clear();
        self.products.clear();
        if n < 40 || returns.iter().any(|r| !r.is_finite()) {
            return false;
        }
        let mean = returns.iter().sum::<f64>() / n as f64;
        self.residuals.extend(returns.iter().map(|r| r - mean));
        self.ss = self.residuals.iter().map(|r| r * r).sum();
        // Relative guard: constant returns leave only rounding residue in ss.
        let scale: f64 = returns.iter().map(|r| r * r).sum::<f64>().max(f64::MIN_POSITIVE);
        if self.ss <= n as f64 * (64.0 * f64::EPSILON).powi(2) || self.ss <= 1e-20 * scale {
            return false;
        }
        true
    }
    fn product(&mut self, j: usize) -> f64 {
        while self.products.len() < j {
            let k = self.products.len() + 1;
            let e = &self.residuals;
            self.products.push(e[k..].iter().zip(&e[..e.len() - k]).map(|(a, b)| (a * a) * (b * b)).sum());
        }
        self.products[j - 1]
    }
    fn ratio(&mut self, q: usize) -> VarianceRatio {
        let n = self.residuals.len();
        let mut out = VarianceRatio { horizon: q, returns: n, ratio: None, z_star: None };
        if q < 2 || n < q.saturating_mul(20) {
            return out;
        }
        let ss = self.ss;
        // σ_a² = Σe²/(N−1);  σ_c² = Σ_(windows) (Σ_q e)² / m,  m = q(N−q+1)(1−q/N).
        let sigma_one = ss / (n - 1) as f64;
        let m = q as f64 * (n - q + 1) as f64 * (1.0 - q as f64 / n as f64);
        let mut window: f64 = self.residuals[..q].iter().sum();
        let mut sum_sq = window * window;
        for t in q..n {
            window += self.residuals[t] - self.residuals[t - q];
            sum_sq += window * window;
        }
        let vr = (sum_sq / m) / sigma_one;
        // delta_j = N sum(e_t² e_(t-j)²)/(sum e_t²)²;
        // theta = sum [2(q-j)/q]² delta_j; z* = sqrt(N)(VR-1)/sqrt(theta).
        // Cancel N analytically to avoid an extra factor of sqrt(N).
        let variance: f64 =
            (1..q).map(|j| (2.0 * (q - j) as f64 / q as f64).powi(2) * self.product(j) / (ss * ss)).sum();
        out.ratio = vr.is_finite().then_some(vr);
        out.z_star = (variance > 0.0).then(|| (vr - 1.0) / variance.sqrt()).filter(|z| z.is_finite());
        out
    }
    fn family(&mut self, returns: &[f64]) -> [VarianceRatio; 3] {
        if self.prepare(returns) {
            HORIZONS.map(|q| self.ratio(q))
        } else {
            HORIZONS.map(|q| VarianceRatio { horizon: q, returns: returns.len(), ratio: None, z_star: None })
        }
    }
}

/// Trending (grey) verdict for one VR family: any VR>1 with z* above the
/// Bonferroni critical value.
fn trending(vr: &[VarianceRatio; 3]) -> bool {
    vr.iter().any(|v| v.ratio.is_some_and(|r| r > 1.0) && v.z_star.is_some_and(|z| z > CRITICAL_Z))
}

/// Point-in-time band trust: entry t uses only closes `0..=t` (the trailing
/// `DIAGNOSTIC_BARS` window ending at t). `true` = grey (trending or VR unavailable).
/// Entries at or after `closes.len()` do not exist; callers pad them.
pub fn muted_series(closes: &[f64]) -> Vec<bool> {
    let returns: Vec<_> = closes.windows(2).map(|w| w[1].ln() - w[0].ln()).collect();
    let need = HORIZONS.iter().max().copied().unwrap_or(2) * 20;
    let mut core = VrCore::default();
    (0..closes.len())
        .map(|t| {
            // Returns ending at close t: returns[t−1] is ln C_t − ln C_(t−1).
            if t < need {
                return true;
            }
            let start = (t + 1).saturating_sub(DIAGNOSTIC_BARS);
            let vr = core.family(&returns[start..t]);
            trending(&vr) || vr.iter().any(|v| v.z_star.is_none())
        })
        .collect()
}

/// Complementary error function (Numerical Recipes erfcc, |rel err| < 1.2e-7).
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let r = t * (-z * z - 1.26551223
        + t * (1.00002368
            + t * (0.37409196
                + t * (0.09678418
                    + t * (-0.18628806
                        + t * (0.27886807 + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277)))))))))
        .exp();
    if x >= 0.0 { r } else { 2.0 - r }
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
    /// Latest closed snapshot verdict (drives the label text).
    pub trending: bool,
    /// Point-in-time grey flag per chart bar: entry t uses closes up to t only.
    /// Index-aligned with the input bars; the forming slot repeats the last closed flag.
    pub muted_at: Vec<bool>,
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
            "CURRENT CLOSED SNAPSHOT · {} … {} UTC · {} closed bars\n{}\nVR/z*: Lo–MacKinlay overlapping, bias-corrected VR; heteroskedasticity-robust z*. q2,5,10 on up to 512 closes; ≥20q log returns each. Two-sided Bonferroni family 5%, |z*|>{CRITICAL_Z}. Failure to reject a random walk is not mean reversion.\nEmpirical band tails: rolling in-window price z (includes the observation); descriptive, overlapping and not independent. n={} finite z.\n",
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
        for v in &self.vr {
            let fmt = |x: Option<f64>, d: usize| x.map(|x| format!("{x:.d$}")).unwrap_or_else(|| "—".into());
            out.push_str(&format!(
                "q{}: N={} · VR {} · robust z* {} (two-sided p {}) · homoskedastic z {}\n",
                v.horizon,
                v.returns,
                fmt(v.ratio, 3),
                fmt(v.z_star, 2),
                fmt(v.p_two_sided(), 4),
                fmt(homoskedastic_z(v), 2)
            ));
        }
        out.push_str("Grey styling is point-in-time: each bar's colour uses only the trailing ≤512 closes up to that bar (grey = trend persistence or VR unavailable). The text above is the latest closed snapshot. Sigma is price-level sample standard deviation, not return volatility. The forming bar is excluded: bands end at the last closed bar.");
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
    let vr = VrCore::default().family(&returns);
    let (tail_n, tail_counts) = tails(&band.z[start..]);
    let trending = trending(&vr);
    let mut muted_at = muted_series(&closes);
    // The forming slot has no band; carry the last closed verdict so the
    // index-aligned series never reads past the end.
    let last = muted_at.last().copied().unwrap_or(true);
    muted_at.resize(bars.len(), last);
    let mut report = Report {
        closed,
        first_ts: bars[start].ts,
        last_ts: bars[closed - 1].ts,
        vr,
        tail_n,
        tail_counts,
        trending,
        muted_at,
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
    fn vr_hand_computed_period_four_vector() {
        // e = a,a,−a,−a,… (N=40, mean 0). Σe²=40a²; 39 two-step sums alternate
        // 2a,0,−2a,0 → Σ=20·4a²=80a²; m=2·39·(1−2/40)=74.1.
        // VR=(80/74.1)/(40/39)=20/19. δ1 numerator=39a⁴, (Σe²)²=1600a⁴ →
        // z*=(1/19)/√(39/1600)=40/(19√39). Homoskedastic φ(2)=1 → z=√40/19.
        let a = 0.01;
        let r: Vec<_> = (0..40).map(|i| if i % 4 < 2 { a } else { -a }).collect();
        let v = variance_ratio(&r, 2);
        assert!((v.ratio.unwrap() - 20.0 / 19.0).abs() < 1e-12, "{v:?}");
        assert!((v.z_star.unwrap() - 40.0 / (19.0 * 39.0_f64.sqrt())).abs() < 1e-10);
        assert!((homoskedastic_z(&v).unwrap() - 40.0_f64.sqrt() / 19.0).abs() < 1e-10);
        // 39 < 20q for q=2 is refused; 40 is the minimum.
        assert!(variance_ratio(&r[..39], 2).ratio.is_none());
    }
    #[test]
    fn vr_matches_brute_force_lo_mackinlay_reference() {
        let r = series(700, 0.3);
        let n = r.len() as f64;
        let mean = r.iter().sum::<f64>() / n;
        let e: Vec<_> = r.iter().map(|x| x - mean).collect();
        let ss: f64 = e.iter().map(|x| x * x).sum();
        for q in [2, 5, 10, 17] {
            let m = q as f64 * (n - q as f64 + 1.0) * (1.0 - q as f64 / n);
            let sq = e.windows(q).map(|w| w.iter().sum::<f64>().powi(2)).sum::<f64>() / m;
            let vr = sq / (ss / (n - 1.0));
            let theta: f64 = (1..q)
                .map(|j| {
                    let delta = n * (j..e.len()).map(|t| e[t].powi(2) * e[t - j].powi(2)).sum::<f64>() / (ss * ss);
                    (2.0 * (q - j) as f64 / q as f64).powi(2) * delta
                })
                .sum();
            let z = n.sqrt() * (vr - 1.0) / theta.sqrt();
            let v = variance_ratio(&r, q);
            assert!((v.ratio.unwrap() - vr).abs() < 1e-12, "q{q}");
            assert!((v.z_star.unwrap() - z).abs() < 1e-9, "q{q}");
        }
    }
    #[test]
    fn two_sided_p_values_match_normal_quantiles() {
        let at = |z: f64| VarianceRatio { horizon: 2, returns: 100, ratio: Some(1.0), z_star: Some(z) }.p_two_sided();
        for (z, p) in [(0.0, 1.0), (1.959964, 0.05), (-1.959964, 0.05), (2.575829, 0.01), (CRITICAL_Z, 0.05 / 3.0)] {
            assert!((at(z).unwrap() - p).abs() < 2e-5, "z={z}: {:?}", at(z));
        }
        assert_eq!(VarianceRatio { horizon: 2, returns: 1, ratio: None, z_star: None }.p_two_sided(), None);
    }
    #[test]
    fn grey_is_point_in_time_not_the_latest_snapshot() {
        // 700 random-walk returns, then 700 strongly persistent AR(1) returns.
        let mut r = series(700, 0.0);
        r.extend(series(700, 0.8).into_iter().map(|x| x * 1.3));
        let b = fixture(&r);
        let (_, report) = compute(&b, Timeframe::D1, AssetClass::Crypto, None, 20, &[]).unwrap();
        // The latest snapshot is trending, so the old global styling greyed everything.
        assert!(report.trending);
        assert_eq!(report.muted_at.len(), b.len());
        assert!(*report.muted_at.last().unwrap());
        // Warm-up (fewer than 200 returns) is grey: VR unavailable.
        assert!(report.muted_at[..200].iter().all(|m| *m));
        // Inside the random-walk era most bars are coloured.
        let early = &report.muted_at[250..700];
        assert!(early.iter().filter(|m| !**m).count() > early.len() * 3 / 4, "random walk era mostly trusted");
        // Deep in the persistent era (window fully AR) bars are grey.
        assert!(report.muted_at[1250..1400].iter().all(|m| *m));
        // Causality: truncating the future never changes an earlier flag.
        let closes: Vec<_> = b.iter().map(|b| b.close).collect();
        let full = muted_series(&closes);
        for k in [201, 450, 699, 900, 1200] {
            assert_eq!(muted_series(&closes[..k]), full[..k], "prefix {k}");
        }
        // The last closed flag agrees with the snapshot verdict.
        let closed = report.closed;
        assert_eq!(report.muted_at[closed - 1], report.muted());
    }
    #[test]
    #[ignore = "timing benchmark"]
    fn muted_series_bench() {
        let b = fixture(&series(20_000, 0.2));
        let closes: Vec<_> = b.iter().map(|b| b.close).collect();
        let t = std::time::Instant::now();
        let m = muted_series(&closes);
        eprintln!("BENCH point-in-time Honest Z on {} bars: {:.1} ms", m.len(), t.elapsed().as_secs_f64() * 1000.0);
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
