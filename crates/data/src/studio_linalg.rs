//! Small dense linear algebra for the Regression Lab: standardised design, Householder QR with
//! column pivoting, back-substitution, the explicit upper-triangular inverse, κ∞ and VIF.
//! Column-major flat storage; nothing here allocates per row. Never forms XᵀX.
//!
//! Z = [1 | (x_j − μ_j)/s_j] with *training-only* μ, s (population). Standardising makes κ and the
//! rank test invariant to column scale and shift, and makes the intercept orthogonal to every
//! factor column (so VIF_j = n·Σ_m (R⁻¹)²_{π⁻¹(j),m} = diag(Corr⁻¹)_j).

/// Relative pivot size at or below which a column is treated as linearly dependent.
pub const RANK_TOL: f64 = 1e-7;
/// κ∞(R) above this refuses the fit.
pub const KAPPA_MAX: f64 = 1e6;

#[derive(Debug, Clone, PartialEq)]
pub enum LinalgError {
    /// Needs n > k (rows strictly exceed parameters, so σ² has a degree of freedom).
    TooFewRows { n: usize, k: usize },
    /// Zero-variance factor; `column` indexes Z (0 = intercept, j + 1 = factor j).
    Constant { column: usize },
    /// `rank` of k independent columns; `column` is the first pivoted-out Z column.
    RankDeficient { rank: usize, k: usize, column: usize },
    IllConditioned { kappa: f64 },
    NonFinite,
}
impl std::fmt::Display for LinalgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewRows { n, k } => write!(f, "need more rows than parameters (n={n}, k={k})"),
            Self::Constant { column } => write!(f, "factor {column} is constant in the training span"),
            Self::RankDeficient { rank, k, column } => {
                write!(f, "rank-deficient design: rank {rank} of {k}; column {column} is linearly dependent")
            }
            Self::IllConditioned { kappa } => {
                write!(f, "ill-conditioned design: κ∞={kappa:.3e} exceeds {KAPPA_MAX:.0e}")
            }
            Self::NonFinite => write!(f, "non-finite input"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OlsFit {
    /// Original units, intercept first.
    pub coefficients: Vec<f64>,
    /// Coefficients on Z (intercept first).
    pub gamma: Vec<f64>,
    pub mean: Vec<f64>,
    pub scale: Vec<f64>,
    pub fitted: Vec<f64>,
    pub residuals: Vec<f64>,
    pub sse: f64,
    /// √(SSE/(n−k)).
    pub sigma: f64,
    pub rank: usize,
    pub kappa_inf: f64,
    /// One per factor.
    pub vif: Vec<f64>,
}

/// κ∞(R) = ‖R‖∞·‖R⁻¹‖∞ for an upper-triangular k×k `r` (column-major). Infinite when singular.
pub fn kappa_inf(r: &[f64], k: usize) -> f64 {
    let Some(inv) = upper_inverse(r, k) else { return f64::INFINITY };
    row_sum_norm(r, k) * row_sum_norm(&inv, k)
}

fn row_sum_norm(a: &[f64], k: usize) -> f64 {
    (0..k).map(|i| (0..k).map(|c| a[c * k + i].abs()).sum::<f64>()).fold(0., f64::max)
}

/// Explicit inverse of an upper-triangular matrix by column back-substitution.
fn upper_inverse(r: &[f64], k: usize) -> Option<Vec<f64>> {
    let mut inv = vec![0.; k * k];
    for c in 0..k {
        // Solve R x = e_c.
        for i in (0..=c).rev() {
            let mut s = f64::from(i == c);
            for m in i + 1..=c {
                s -= r[m * k + i] * inv[c * k + m];
            }
            let d = r[i * k + i];
            if d == 0. || !d.is_finite() {
                return None;
            }
            inv[c * k + i] = s / d;
        }
    }
    inv.iter().all(|v| v.is_finite()).then_some(inv)
}

/// OLS with an intercept on `factors` (columns) and `y`, via pivoted QR on standardised columns.
pub fn fit_ols(factors: &[&[f64]], y: &[f64]) -> Result<OlsFit, LinalgError> {
    let (n, p) = (y.len(), factors.len());
    let k = p + 1;
    if n <= k {
        return Err(LinalgError::TooFewRows { n, k });
    }
    if y.iter().any(|v| !v.is_finite()) || factors.iter().any(|c| c.len() != n || c.iter().any(|v| !v.is_finite())) {
        return Err(LinalgError::NonFinite);
    }
    let mut mean = Vec::with_capacity(p);
    let mut scale = Vec::with_capacity(p);
    // Z, column-major n×k.
    let mut a = vec![1.; n * k];
    for (j, col) in factors.iter().enumerate() {
        let mu = col.iter().sum::<f64>() / n as f64;
        let s = (col.iter().map(|v| (v - mu) * (v - mu)).sum::<f64>() / n as f64).sqrt();
        let magnitude = col.iter().map(|v| v.abs()).fold(0., f64::max);
        if !s.is_finite() || s <= 64. * f64::EPSILON * magnitude {
            return Err(LinalgError::Constant { column: j + 1 });
        }
        for (i, v) in col.iter().enumerate() {
            a[(j + 1) * n + i] = (v - mu) / s;
        }
        mean.push(mu);
        scale.push(s);
    }
    let z = a.clone();
    let mut qty = y.to_vec();
    let mut perm: Vec<usize> = (0..k).collect();
    for i in 0..k {
        // Largest remaining column norm; strict `>` with a relative guard keeps the lowest index on ties.
        let norm2 = |a: &[f64], c: usize| a[c * n + i..(c + 1) * n].iter().map(|v| v * v).sum::<f64>();
        let (mut best, mut best_norm) = (i, norm2(&a, i));
        for c in i + 1..k {
            let v = norm2(&a, c);
            if v > best_norm * (1. + 1e-12) {
                (best, best_norm) = (c, v);
            }
        }
        if best != i {
            for r in 0..n {
                a.swap(i * n + r, best * n + r);
            }
            perm.swap(i, best);
        }
        // Householder reflection zeroing a[i+1.., i].
        let norm = best_norm.max(0.).sqrt();
        if norm == 0. {
            continue;
        }
        let aii = a[i * n + i];
        let alpha = if aii >= 0. { -norm } else { norm };
        let mut v: Vec<f64> = a[i * n + i..(i + 1) * n].to_vec();
        v[0] -= alpha;
        let vnorm2 = v.iter().map(|x| x * x).sum::<f64>();
        if vnorm2 > 0. {
            for c in i + 1..k {
                let dot = a[c * n + i..(c + 1) * n].iter().zip(&v).map(|(x, w)| x * w).sum::<f64>();
                let f = 2. * dot / vnorm2;
                for (x, w) in a[c * n + i..(c + 1) * n].iter_mut().zip(&v) {
                    *x -= f * w;
                }
            }
            let dot = qty[i..].iter().zip(&v).map(|(x, w)| x * w).sum::<f64>();
            let f = 2. * dot / vnorm2;
            for (x, w) in qty[i..].iter_mut().zip(&v) {
                *x -= f * w;
            }
        }
        a[i * n + i] = alpha;
        for r in i + 1..n {
            a[i * n + r] = 0.;
        }
    }
    // R, column-major k×k.
    let mut r = vec![0.; k * k];
    for c in 0..k {
        for i in 0..=c {
            r[c * k + i] = a[c * n + i];
        }
    }
    let r00 = r[0].abs();
    let rank = (0..k).filter(|&i| r[i * k + i].abs() > RANK_TOL * r00).count();
    if rank < k {
        // Pivoting orders diagonals by decreasing size, so the first failing position is the culprit.
        let first = (0..k).find(|&i| r[i * k + i].abs() <= RANK_TOL * r00).unwrap_or(rank);
        return Err(LinalgError::RankDeficient { rank, k, column: perm[first] });
    }
    let inv = upper_inverse(&r, k).ok_or(LinalgError::RankDeficient { rank: 0, k, column: perm[0] })?;
    let kappa = row_sum_norm(&r, k) * row_sum_norm(&inv, k);
    if !kappa.is_finite() || kappa > KAPPA_MAX {
        return Err(LinalgError::IllConditioned { kappa });
    }
    // R γ_P = (Qᵀy)[..k], then un-pivot.
    let mut gp = vec![0.; k];
    for i in (0..k).rev() {
        let mut s = qty[i];
        for m in i + 1..k {
            s -= r[m * k + i] * gp[m];
        }
        gp[i] = s / r[i * k + i];
    }
    let mut gamma = vec![0.; k];
    for (i, &c) in perm.iter().enumerate() {
        gamma[c] = gp[i];
    }
    let mut coefficients = vec![0.; k];
    let mut b0 = gamma[0];
    for j in 0..p {
        coefficients[j + 1] = gamma[j + 1] / scale[j];
        b0 -= gamma[j + 1] * mean[j] / scale[j];
    }
    coefficients[0] = b0;
    let mut fitted = vec![gamma[0]; n];
    for j in 0..p {
        for (f, zi) in fitted.iter_mut().zip(&z[(j + 1) * n..(j + 2) * n]) {
            *f += gamma[j + 1] * zi;
        }
    }
    let residuals: Vec<f64> = y.iter().zip(&fitted).map(|(y, f)| y - f).collect();
    let sse = residuals.iter().map(|e| e * e).sum::<f64>();
    let mut pos = vec![0; k];
    for (i, &c) in perm.iter().enumerate() {
        pos[c] = i;
    }
    let vif = (1..k).map(|c| n as f64 * (0..k).map(|m| inv[m * k + pos[c]].powi(2)).sum::<f64>()).collect();
    Ok(OlsFit {
        coefficients,
        gamma,
        mean,
        scale,
        fitted,
        residuals,
        sse,
        sigma: (sse / (n - k) as f64).sqrt(),
        rank,
        kappa_inf: kappa,
        vif,
    })
}

/// Rows `[1, z_1..z_p]` for `factors` under the standardisation frozen in `fit`.
pub fn standardized_rows(fit: &OlsFit, factors: &[&[f64]]) -> Vec<Vec<f64>> {
    let n = factors.first().map_or(0, |c| c.len());
    (0..n)
        .map(|i| {
            let mut row = Vec::with_capacity(factors.len() + 1);
            row.push(1.);
            for (j, c) in factors.iter().enumerate() {
                row.push((c[i] - fit.mean[j]) / fit.scale[j]);
            }
            row
        })
        .collect()
}

/// V = T·Vz·Tᵀ with T₀₀ = 1, T₀ⱼ = −μ_j/s_j, T_jj = 1/s_j: the covariance of the original-unit
/// coefficients from the covariance of the Z coefficients.
pub fn unscale_covariance(vz: &[Vec<f64>], mean: &[f64], scale: &[f64]) -> Vec<Vec<f64>> {
    let k = vz.len();
    let mut t = vec![vec![0.; k]; k];
    t[0][0] = 1.;
    for j in 1..k {
        t[0][j] = -mean[j - 1] / scale[j - 1];
        t[j][j] = 1. / scale[j - 1];
    }
    let mut tv = vec![vec![0.; k]; k];
    for i in 0..k {
        for c in 0..k {
            tv[i][c] = (i..k).map(|m| t[i][m] * vz[m][c]).sum();
        }
    }
    let mut out = vec![vec![0.; k]; k];
    for i in 0..k {
        for j in 0..k {
            out[i][j] = (j..k).map(|m| tv[i][m] * t[j][m]).sum();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block8() -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let x1: Vec<f64> = [-1., -1., 1., 1.].repeat(2);
        let x2: Vec<f64> = [-1., 1., -1., 1.].repeat(2);
        let y = x1.iter().zip(&x2).map(|(a, b)| 1. + 2. * a - 3. * b + a * b).collect();
        (x1, x2, y)
    }
    fn close(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() <= tol * b.abs().max(1.), "{a} != {b}");
    }

    #[test]
    fn lab_qr_orthogonal_hand_vector() {
        let (x1, x2, y) = block8();
        assert_eq!(y, [3., -5., 5., 1.].repeat(2));
        let f = fit_ols(&[&x1, &x2], &y).unwrap();
        for (got, want) in f.coefficients.iter().zip([1., 2., -3.]) {
            close(*got, want, 1e-12);
        }
        for (got, want) in f.residuals.iter().zip([1., -1., -1., 1., 1., -1., -1., 1.]) {
            close(*got, want, 1e-12);
        }
        close(f.sse, 8., 1e-12);
        close(f.sigma * f.sigma, 8. / 5., 1e-12);
        assert_eq!(f.rank, 3);
        close(f.kappa_inf, 1., 1e-12);
        close(f.vif[0], 1., 1e-12);
        close(f.vif[1], 1., 1e-12);
    }

    fn corr_pair() -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let x1 = vec![1., 1., 1., 1., -1., -1., -1., -1.];
        let x2 = vec![1., 1., 1., -1., 1., -1., -1., -1.];
        let y = vec![0.3, -0.2, 1.1, 0.4, -0.7, 0.9, 0.05, -0.33];
        (x1, x2, y)
    }

    #[test]
    fn lab_vif_known_answer() {
        let (x1, x2, y) = corr_pair();
        let f = fit_ols(&[&x1, &x2], &y).unwrap();
        close(f.vif[0], 4. / 3., 1e-12);
        close(f.vif[1], 4. / 3., 1e-12);
        close(f.kappa_inf, (3. + 3f64.sqrt()) / 2., 1e-12);
    }

    #[test]
    fn lab_condition_guard_scale_invariant() {
        let (x1, x2, y) = corr_pair();
        let kappa = (3. + 3f64.sqrt()) / 2.;
        let s1: Vec<f64> = x1.iter().map(|v| v * 1e-4 + 100.).collect();
        let s2: Vec<f64> = x2.iter().map(|v| v * 1e3).collect();
        let f = fit_ols(&[&s1, &s2], &y).unwrap();
        close(f.kappa_inf, kappa, 1e-12);
        // V4: nearly collinear pair.
        let a: Vec<f64> = [1., 1., -1., -1.].repeat(2);
        let w: Vec<f64> = [1., -1., 1., -1.].repeat(2);
        let y: Vec<f64> = (0..8).map(|i| (i as f64 * 0.7).sin()).collect();
        let near = |eps: f64| -> Vec<f64> { a.iter().zip(&w).map(|(a, w)| a + eps * w).collect() };
        match fit_ols(&[&a, &near(1e-6)], &y) {
            Err(LinalgError::IllConditioned { kappa }) => {
                assert!(kappa > 1e6 && (kappa - 2.000002e6).abs() / 2e6 < 1e-4, "{kappa}");
            }
            other => panic!("{other:?}"),
        }
        // Same pair at wildly different column scales: identical verdict.
        let big: Vec<f64> = a.iter().map(|v| v * 1e8).collect();
        let tiny: Vec<f64> = near(1e-6).iter().map(|v| v * 1e-8).collect();
        assert!(matches!(fit_ols(&[&big, &tiny], &y), Err(LinalgError::IllConditioned { .. })));
        assert!(matches!(fit_ols(&[&a, &near(1e-8)], &y), Err(LinalgError::RankDeficient { .. })));
    }

    #[test]
    fn lab_rank_deficient_unavailable() {
        let a: Vec<f64> = [1., 1., -1., -1.].repeat(2);
        let w: Vec<f64> = [1., -1., 1., -1.].repeat(2);
        let lin: Vec<f64> = a.iter().map(|v| 3. * v + 2.).collect();
        let y: Vec<f64> = (0..8).map(|i| (i as f64 * 0.7).sin() + w[i]).collect();
        match fit_ols(&[&a, &lin], &y) {
            Err(LinalgError::RankDeficient { rank, k, .. }) => assert_eq!((rank, k), (2, 3)),
            other => panic!("{other:?}"),
        }
        assert!(matches!(fit_ols(&[&a, &[5.; 8]], &y), Err(LinalgError::Constant { column: 2 })));
        // n ≤ k: three rows for an intercept and two factors.
        assert!(matches!(fit_ols(&[&a[..3], &w[..3]], &y[..3]), Err(LinalgError::TooFewRows { n: 3, k: 3 })));
        assert!(matches!(fit_ols(&[&[f64::NAN; 8][..]], &y), Err(LinalgError::NonFinite)));
    }

    #[test]
    fn unscale_is_identity_for_standard_columns() {
        let v = vec![vec![2., 0.5], vec![0.5, 3.]];
        assert_eq!(unscale_covariance(&v, &[0.], &[1.]), v);
        let u = unscale_covariance(&v, &[2.], &[4.]);
        close(u[1][1], 3. / 16., 1e-15);
        close(u[0][1], 0.5 / 4. - 2. * 3. / 16., 1e-15);
        close(u[0][0], 2.25, 1e-15);
    }
}
