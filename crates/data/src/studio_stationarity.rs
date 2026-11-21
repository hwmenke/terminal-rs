//! Augmented Dickey–Fuller check for a scored residual segment: MacKinnon (2010) N=1 response-surface
//! critical values, Schwert's lag bound, a BIC lag search on one common sample, and the AR(1)
//! half-life with its first-order small-sample correction. Pure functions; no loading, no refit.
//!
//! The regression is Δe_t = α + γ e_{t−1} + Σ δ_i Δe_{t−i} + u_t. H0: unit root (γ = 0, the residual
//! is not mean-reverting). The null is rejected when τ = γ̂/se(γ̂) < the critical value.
//! Dickey–Fuller (N = 1) values are used because the frozen coefficients were estimated on a disjoint
//! earlier sample: this is **not** an Engle–Granger cointegration test.
use crate::studio_linalg::{LinalgError, nested_qr};
use serde::{Deserialize, Serialize};

/// MacKinnon (2010) response-surface coefficients, N = 1, constant, for the 1%, 5% and 10% levels:
/// `[β∞, β₁, β₂, β₃]`.
pub const MACKINNON_C: [[f64; 4]; 3] = [
    [-3.43035, -6.5393, -16.786, -79.433],
    [-2.86154, -2.8903, -4.234, -40.040],
    [-2.56677, -1.5384, -2.809, 0.],
];

/// Critical values `[1%, 5%, 10%]` at sample size `t`.
pub fn mackinnon(t: f64) -> [f64; 3] {
    let f = |c: &[f64; 4]| c[0] + c[1] / t + c[2] / (t * t) + c[3] / (t * t * t);
    [f(&MACKINNON_C[0]), f(&MACKINNON_C[1]), f(&MACKINNON_C[2])]
}

/// Schwert (1989) bound ⌊12 (T/100)^{1/4}⌋.
pub fn schwert(t: usize) -> usize {
    (12. * (t as f64 / 100.).powf(0.25)).floor() as usize
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdfResult {
    pub tau: f64,
    pub gamma: f64,
    pub intercept: f64,
    pub sse: f64,
    pub lag: usize,
    pub max_lag: usize,
    pub n_eff: usize,
    /// Length of the segment tested.
    pub t_len: usize,
    /// 1%, 5%, 10% critical values at `n_eff`.
    pub crit: [f64; 3],
    /// First and last timestamps of the segment; filled by the caller.
    pub window: (i64, i64),
}
impl AdfResult {
    /// Strongest level at which the unit root is rejected (1, 5 or 10), if any.
    pub fn rejects_at(&self) -> Option<u8> {
        [(0, 1u8), (1, 5), (2, 10)].into_iter().find(|&(i, _)| self.tau < self.crit[i]).map(|(_, l)| l)
    }
}

/// One ADF regression with `p` lagged differences on the common sample `t = p_max+1 … T−1`.
/// Columns are `[1, Δe_{t−1} … Δe_{t−p}, e_{t−1}]` so γ is the last. Returns
/// `(γ, τ, intercept, SSE, n_eff)`.
pub fn adf_fit(e: &[f64], p: usize, p_max: usize) -> Result<(f64, f64, f64, f64, usize), String> {
    let (cols, y) = common_sample(e, p_max, p, false);
    let n_eff = y.len();
    let refs: Vec<&[f64]> = cols.iter().map(|c| c.as_slice()).collect();
    let qr = nested_qr(&refs, &y).map_err(degenerate)?;
    let k = p + 2;
    let (gamma, tau) = qr.last_coef_t(k);
    let intercept = qr.coefficients(k)[0];
    if !(tau.is_finite() && gamma.is_finite()) {
        return Err("degenerate residual segment".into());
    }
    Ok((gamma, tau, intercept, qr.sse(k), n_eff))
}

fn degenerate(_: LinalgError) -> String {
    "degenerate residual segment".into()
}

/// Columns on the common sample. `search == true` orders `[1, e_{t−1}, Δe lags]` so that the leading
/// `p+2` columns are the p-lag model; otherwise `[1, Δe lags, e_{t−1}]`.
fn common_sample(e: &[f64], p_max: usize, p: usize, search: bool) -> (Vec<Vec<f64>>, Vec<f64>) {
    let t_len = e.len();
    let rows = p_max + 1..t_len;
    let d = |t: usize| e[t] - e[t - 1];
    let y: Vec<f64> = rows.clone().map(d).collect();
    let ones = vec![1.; y.len()];
    let level: Vec<f64> = rows.clone().map(|t| e[t - 1]).collect();
    let lag = |i: usize| -> Vec<f64> { rows.clone().map(|t| d(t - i)).collect() };
    let mut cols = vec![ones];
    if search {
        cols.push(level);
        cols.extend((1..=p).map(lag));
    } else {
        cols.extend((1..=p).map(lag));
        cols.push(level);
    }
    (cols, y)
}

/// Full ADF: Schwert bound, BIC over `p = 0..=p_max` on one common sample, then the final regression
/// at the BIC lag on that same sample.
pub fn adf(e: &[f64]) -> Result<AdfResult, String> {
    let t_len = e.len();
    let p_max = schwert(t_len);
    if t_len < p_max + 4 + p_max {
        return Err("segment too short for the lag bound".into());
    }
    let (cols, y) = common_sample(e, p_max, p_max, true);
    let n_eff = y.len();
    let refs: Vec<&[f64]> = cols.iter().map(|c| c.as_slice()).collect();
    let qr = nested_qr(&refs, &y).map_err(degenerate)?;
    let n = n_eff as f64;
    let mut best: Option<(usize, f64)> = None;
    for p in 0..=p_max {
        let sse = qr.sse(p + 2);
        if !(sse > 0.) {
            return Err("degenerate residual segment".into());
        }
        let bic = (sse / n).ln() + (p + 2) as f64 * n.ln() / n;
        if best.is_none_or(|(_, b)| bic < b) {
            best = Some((p, bic));
        }
    }
    let (lag, _) = best.ok_or("no lag candidate")?;
    let (gamma, tau, intercept, sse, n_final) = adf_fit(e, lag, p_max)?;
    debug_assert_eq!(n_final, n_eff);
    Ok(AdfResult {
        tau,
        gamma,
        intercept,
        sse,
        lag,
        max_lag: p_max,
        n_eff,
        t_len,
        crit: mackinnon(n_eff as f64),
        window: (0, 0),
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HalfLife {
    pub phi_hat: f64,
    /// First-order corrected φ̃ = (nφ̂+1)/(n−3).
    pub phi: f64,
    pub n: usize,
    pub bars: Option<f64>,
    pub reason: Option<String>,
}

/// Half-life from the OLS AR(1) coefficient over `n` pairs.
pub fn half_life_from(phi_hat: f64, n: usize) -> HalfLife {
    let phi = (n as f64 * phi_hat + 1.) / (n as f64 - 3.);
    if phi > 0. && phi < 1. {
        HalfLife { phi_hat, phi, n, bars: Some(-std::f64::consts::LN_2 / phi.ln()), reason: None }
    } else {
        HalfLife { phi_hat, phi, n, bars: None, reason: Some(format!("no finite half-life (φ̃ = {phi:.2})")) }
    }
}

/// AR(1) half-life of a contiguous segment: e_t on `[1, e_{t−1}]`.
pub fn half_life(e: &[f64]) -> Result<HalfLife, String> {
    let n = e.len().saturating_sub(1);
    if n < 5 {
        return Err("segment too short".into());
    }
    let (x, y) = (&e[..n], &e[1..]);
    let (mx, my) = (x.iter().sum::<f64>() / n as f64, y.iter().sum::<f64>() / n as f64);
    let sxx: f64 = x.iter().map(|v| (v - mx) * (v - mx)).sum();
    if !(sxx > 0.) {
        return Err("degenerate residual segment".into());
    }
    let sxy: f64 = x.iter().zip(y).map(|(a, b)| (a - mx) * (b - my)).sum();
    Ok(half_life_from(sxy / sxx, n))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn close(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() <= tol, "{a} != {b}");
    }

    #[test]
    fn residual_adf_hand_statistic() {
        // V4: Δe = [1,−1,−1,1] on e₋₁ = [0,1,0,−1].
        let e = [0., 1., 0., -1., 0.];
        let (gamma, tau, intercept, sse, n_eff) = adf_fit(&e, 0, 0).unwrap();
        assert_eq!(n_eff, 4);
        close(intercept, 0., 1e-12);
        close(gamma, -1., 1e-12);
        close(sse, 2., 1e-12);
        close(tau, -(2f64.sqrt()), 1e-12);
        // V5.
        let c = mackinnon(100.);
        for (got, want) in c.iter().zip([-3.497501033, -2.890906440, -2.582434900]) {
            close(*got, want, 1e-9);
        }
        // V6.
        let c = mackinnon(49.);
        for (got, want) in c.iter().zip([-3.571471523, -2.922629480, -2.599335847]) {
            close(*got, want, 1e-6);
        }
        // V7.
        for (t, p) in [(60, 10), (100, 12), (250, 15), (2000, 25)] {
            assert_eq!(schwert(t), p, "T={t}");
        }
    }

    #[test]
    fn adf_rejects_an_ar1_and_keeps_a_random_walk() {
        let mut s = 7u64;
        let mut noise = move || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((s >> 33) as f64 / (1u64 << 31) as f64) - 0.5
        };
        let mut ar = vec![0.];
        let mut rw = vec![0.];
        for _ in 0..400 {
            let n = noise();
            ar.push(0.5 * ar.last().unwrap() + n);
            rw.push(rw.last().unwrap() + n);
        }
        let a = adf(&ar).unwrap();
        assert_eq!(a.rejects_at(), Some(1), "tau {}", a.tau);
        assert!(a.lag <= a.max_lag && a.n_eff == 401 - 1 - a.max_lag);
        let w = adf(&rw).unwrap();
        assert!(w.rejects_at().is_none() || w.rejects_at() == Some(10), "tau {}", w.tau);
        assert!(adf(&[1.; 80]).is_err());
    }

    #[test]
    fn residual_half_life_vector() {
        let h = half_life_from(0.5, 100);
        close(h.phi, 51. / 97., 1e-15);
        close(h.bars.unwrap(), 1.078181646, 1e-9);
        let h = half_life_from(0.99, 100);
        assert!(h.phi >= 1. && h.bars.is_none() && h.reason.unwrap().contains("φ̃ = 1."));
        assert!(half_life_from(-0.1, 100).bars.is_none());
    }
}
