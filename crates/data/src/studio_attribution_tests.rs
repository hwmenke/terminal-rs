//! Known answers, identity, stride/endpoint, SE, label, key and grammar tests for ATTR.
use super::*;
use crate::studio_corr::NamedBars;
use crate::studio_inference::{CovarianceMethod, RegressionCovariance};
use crate::studio_linalg::FIT_CALLS;
use crate::studio_lab::FactorSpec;
use crate::studio_residual::{FrozenModel, ResidualReport, score, score_key, span_digest};
use tr_core::Bar;

const DAY: i64 = 86_400;
const T0: i64 = 1_699_920_000;
const TGT: &str = "TTT Curncy";
const F1: &str = "FFF Curncy";
const F2: &str = "GGG Curncy";
const F3: &str = "HHH Curncy";

fn lcg(s: &mut u64) -> f64 {
    *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*s >> 11) as f64 / (1u64 << 53) as f64
}
fn walk(n: usize, seed: u64) -> Vec<f64> {
    let mut s = seed;
    let mut p = 100.;
    (0..n)
        .map(|_| {
            p *= 1. + 0.02 * (lcg(&mut s) - 0.5);
            p
        })
        .collect()
}
fn bar(ts: i64, c: f64) -> Bar {
    Bar::new(ts, c, c, c, c, 1.).with_provenance(Provenance::Real)
}
fn named(symbol: &str, closes: &[f64]) -> NamedBars {
    NamedBars {
        symbol: symbol.into(),
        bars: closes.iter().enumerate().map(|(d, c)| bar(T0 + d as i64 * DAY, *c)).collect(),
        source: "unit".into(),
        ..Default::default()
    }
}
fn drop_day(mut r: NamedBars, day: usize) -> NamedBars {
    r.bars.remove(day);
    r
}
fn close(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol * b.abs().max(1.), "{a} != {b}");
}
fn ts_of(day: usize) -> i64 {
    T0 + day as i64 * DAY
}

fn cov(method: CovarianceMethod, v: Vec<Vec<f64>>) -> RegressionCovariance {
    RegressionCovariance { method, lag: 0, n: 120, covariance: Some(v), fallback: false, unavailable: None }
}
fn four(v: &[Vec<f64>]) -> Vec<RegressionCovariance> {
    [CovarianceMethod::Classical, CovarianceMethod::HC3, CovarianceMethod::HacFixed5, CovarianceMethod::HacAutomatic]
        .into_iter()
        .map(|m| cov(m, v.to_vec()))
        .collect()
}
fn ident(k: usize, s: f64) -> Vec<Vec<f64>> {
    (0..k).map(|i| (0..k).map(|j| if i == j { s } else { 0. }).collect()).collect()
}
fn refresh(fit: &mut FrozenFit) {
    fit.content_digest = fit.compute_digest();
    fit.validate().unwrap();
}

/// A hand-built frozen model (no fit anywhere): `ŷ = β·[1, x…]`, trained through day `trained_idx`.
fn mk_fit(target: Transform, facs: &[(Transform, usize)], live: &[&NamedBars], beta: &[f64], sigma: f64, trained_idx: usize) -> FrozenFit {
    let k = beta.len();
    let thru = ts_of(trained_idx);
    let h = target.horizon();
    let mut s = 11u64;
    let train: Vec<f64> = (0..120).map(|_| sigma * (lcg(&mut s) - 0.5) * 3.4).collect();
    let mut breaks = vec![false; 120];
    breaks[0] = true;
    let mut fit = FrozenFit {
        id: "t-v1".into(),
        name: "t".into(),
        saved_at: 1,
        model: FrozenModel {
            symbol: TGT.into(),
            target,
            factors: live[1..]
                .iter()
                .zip(facs)
                .map(|(r, (t, l))| FactorSpec { symbol: r.symbol.clone(), transform: *t, lag: *l })
                .collect(),
            coefficients: beta.to_vec(),
            n_train: 120,
            k,
            train_start: T0,
            ..Default::default()
        },
        residual_mean: 0.,
        residual_sigma: sigma,
        trained_thru: thru,
        embargo_bars: h - 1,
        training_receipts: live
            .iter()
            .map(|r| SeriesReceipt {
                source: r.source.clone(),
                provenance: Provenance::Real,
                observed_at: None,
                closed_thru: 0,
                input_digest: 0,
                gaps: vec![],
            })
            .collect(),
        span_digests: live.iter().map(|r| span_digest(&r.bars, &r.symbol, T0, thru)).collect(),
        train_residuals: train,
        train_breaks: breaks,
        monitoring_horizon: 20,
        bootstrap_block: 5,
        ..Default::default()
    };
    refresh(&mut fit);
    fit
}
fn run(fit: &FrozenFit, t: &NamedBars, f: &[NamedBars]) -> ResidualReport {
    score(fit, t, f, true, "unit", 0, None, 1)
}
const R1: Transform = Transform::LogReturn { h: 1 };
const C1: Transform = Transform::Change { h: 1 };

/// V5: Change h=1 target 100, 99, 97; F1 50, 51, 53; F2 20, 21, 24; β = [1, 2, −3].
fn v5() -> (FrozenFit, NamedBars, Vec<NamedBars>) {
    let (mut t, mut f1, mut f2) = (walk(40, 1), walk(40, 2), walk(40, 3));
    t[37..].copy_from_slice(&[100., 99., 97.]);
    f1[37..].copy_from_slice(&[50., 51., 53.]);
    f2[37..].copy_from_slice(&[20., 21., 24.]);
    let (t, f1, f2) = (named(TGT, &t), named(F1, &f1), named(F2, &f2));
    let fit = mk_fit(C1, &[(C1, 0), (C1, 0)], &[&t, &f1, &f2], &[1., 2., -3.], 2., 20);
    (fit, t, vec![f1, f2])
}

#[test]
fn attribution_minus_four_hand_vector() {
    // V1
    let (c, pred, r) = decompose(&[1., 2., -3.], &[2., 3., 4.], -3.);
    assert_eq!(c, vec![2., 6., -12.]);
    assert_eq!((pred, r), (-4., 1.));
    // V5 end to end.
    let (fit, t, f) = v5();
    let rep = run(&fit, &t, &f);
    assert!(rep.unavailable.is_none(), "{:?}", rep.unavailable);
    let a = attribute(&fit, &rep, &AttrWindow::Obs { obs: 2, to: None });
    assert!(a.unavailable.is_none(), "{:?}", a.unavailable);
    assert_eq!(a.m, 2);
    assert_eq!((a.actual, a.predicted, a.residual), (-3., -4., 1.));
    assert_eq!(a.intercept, 2.);
    assert_eq!(a.factors.iter().map(|f| f.contribution).collect::<Vec<_>>(), vec![6., -12.]);
    assert_eq!(a.factors.iter().map(|f| f.exposure).collect::<Vec<_>>(), vec![3., 4.]);
    close(a.residual_scored_sum, 1., 1e-12);
    assert_eq!(a.label, WindowLabel::OutOfSample);
    assert_eq!(a.mode, AttrMode::Sum { h: 1 });
    assert!(a.factors.iter().all(|f| matches!(f.kind, ExposureKind::Move { .. })));
    // Σŷ over the window = −4 (0 + −4) and the final path point equals the cross-section.
    let sum_fitted: f64 = [38, 39].iter().map(|t| rep.fitted[*t].unwrap()).sum();
    close(sum_fitted, -4., 1e-12);
    let last = a.path.last().unwrap();
    close(last.actual, a.actual, 1e-12);
    close(last.residual, a.residual, 1e-12);
    close(last.drift, a.intercept, 1e-12);
    close(last.factors[0], 6., 1e-12);
    // Default window = the latest complete move.
    let one = attribute(&fit, &rep, &AttrWindow::default());
    assert_eq!((one.m, one.actual, one.to_ts), (1, -2., ts_of(39)));
}

#[test]
fn attribution_se_5p8_and_9() {
    let v = ident(3, 0.2);
    let d = [2., 3., 4.];
    let q = quad_form(&d, &v).unwrap();
    close(q, 5.8, 1e-12);
    close(q.sqrt(), 2.408318916, 1e-9);
    let terms: Vec<f64> = d.iter().zip(&v).enumerate().map(|(i, (x, r))| x.abs() * r[i].sqrt()).collect();
    for (t, e) in terms.iter().zip([0.894427191, 1.341640786, 1.788854382]) {
        close(*t, e, 1e-9);
    }
    close(terms.iter().sum::<f64>(), 4.024922359, 1e-9);
    // V3: σ² = 1.6, m = 2, ρ = [] → Var_W = 3.2; predictive² = 9.
    let (w, fb) = residual_variance_sum(2, 1.6, &[]);
    assert!(!fb);
    close(w, 3.2, 1e-12);
    close((q + w).sqrt(), 3., 1e-12);
    // End to end with a stored classical covariance.
    let (mut fit, t, f) = v5();
    fit.coef_covariance = four(&v);
    refresh(&mut fit);
    let rep = run(&fit, &t, &f);
    let a = attribute(&fit, &rep, &AttrWindow::Obs { obs: 2, to: None });
    assert_eq!(a.covariances.len(), 4);
    assert_eq!(a.default_cov, Some(3));
    let c = &a.covariances[0];
    close(c.coefficient_se.unwrap().powi(2), 5.8, 1e-12);
    let rho = rho_hat(&fit.train_residuals, &fit.train_breaks, 1).unwrap().0;
    let (var_w, _) = residual_variance_sum(2, fit.residual_sigma.powi(2), &[rho]);
    close(c.predictive_se.unwrap().powi(2), 5.8 + var_w, 1e-12);
    close(a.residual_var.variance.unwrap(), var_w, 1e-12);
    // Term SEs are |d|√V_jj, never added.
    close(a.factors[0].coefficient_se[0].unwrap(), 3. * 0.2f64.sqrt(), 1e-12);
    close(a.covariances[0].intercept_se.unwrap(), 2. * 0.2f64.sqrt(), 1e-12);
}

#[test]
fn attribution_offdiagonal_covariance_matters() {
    let d = [2., 3., 4.];
    let mut pos = ident(3, 0.2);
    let mut neg = ident(3, 0.2);
    pos[0][1] = 0.1;
    pos[1][0] = 0.1;
    neg[0][1] = -0.1;
    neg[1][0] = -0.1;
    let (qp, qn) = (quad_form(&d, &pos).unwrap(), quad_form(&d, &neg).unwrap());
    close(qp, 7.0, 1e-12);
    close(qn, 4.6, 1e-12);
    close(qp.sqrt(), 2.645751311, 1e-9);
    close(qn.sqrt(), 2.144761059, 1e-9);
    close(qp + 3.2, 10.2, 1e-12);
    close(qn + 3.2, 7.8, 1e-12);
    // Term SEs are identical; the totals are not; and the total ≠ Σ term SEs.
    let term = |v: &[Vec<f64>]| -> Vec<f64> { d.iter().enumerate().map(|(i, x)| x.abs() * v[i][i].sqrt()).collect() };
    assert_eq!(term(&pos), term(&neg));
    assert!((qp.sqrt() - qn.sqrt()).abs() > 0.4);
    assert!((term(&pos).iter().sum::<f64>() - qp.sqrt()).abs() > 1.);
    assert!(quad_form(&d, &[vec![1.]]).is_none());
    assert!(quad_form(&d, &vec![vec![-1.; 3]; 3]).is_none());
}

fn level_pair(n: usize) -> (NamedBars, NamedBars) {
    let mut y = walk(n, 5);
    let mut x = walk(n, 6);
    // x_a = 10, x_b = 13, y_a = 30, y_b = 37 at a = n−4, b = n−1.
    x[n - 4] = 10.;
    x[n - 1] = 13.;
    y[n - 4] = 30.;
    y[n - 1] = 37.;
    (named(TGT, &y), named(F1, &x))
}

#[test]
fn attribution_level_uses_endpoint_difference() {
    // V6 helpers.
    let d = [0., 3.];
    let v = ident(2, 0.2);
    let q = quad_form(&d, &v).unwrap();
    close(q, 1.8, 1e-12);
    close(q.sqrt(), 1.341640786, 1e-9);
    close(residual_variance_endpoint(2., 0.75), 1.0, 1e-12);
    close((q + 1.0).sqrt(), 1.673320053, 1e-9);
    // End to end.
    let (t, f) = level_pair(40);
    let mut fit = mk_fit(Transform::Level, &[(Transform::Level, 0)], &[&t, &f], &[1., 2.], 2., 15);
    fit.coef_covariance = four(&v);
    refresh(&mut fit);
    let rep = run(&fit, &t, &[f.clone()]);
    let w = AttrWindow::Obs { obs: 3, to: None };
    let a = attribute(&fit, &rep, &w);
    assert!(a.unavailable.is_none(), "{:?}", a.unavailable);
    assert_eq!(a.mode, AttrMode::Endpoint);
    assert_eq!((a.intercept, a.factors[0].exposure, a.factors[0].contribution), (0., 3., 6.));
    assert_eq!((a.actual, a.predicted, a.residual), (7., 6., 1.));
    assert_eq!(a.factors[0].kind, ExposureKind::EndpointMove);
    close(a.residual, a.residual_scored_sum, 1e-12);
    assert_eq!(a.drift_share, None);
    // No sum over intermediate rows: changing one intermediate close leaves A, Â and R alone.
    let mut y2 = t.clone();
    y2.bars[38].close = 99.;
    y2.bars[38].open = 99.;
    y2.bars[38].high = 99.;
    y2.bars[38].low = 99.;
    let rep2 = run(&fit, &y2, &[f.clone()]);
    let b = attribute(&fit, &rep2, &w);
    assert_eq!((b.actual, b.predicted, b.residual), (a.actual, a.predicted, a.residual));
    assert_ne!(a.path, b.path);
}

#[test]
fn attribution_log_units_not_simple_sum() {
    let mut t = walk(40, 7);
    t[37..].copy_from_slice(&[100., 110., 99.]);
    let (t, f1) = (named(TGT, &t), named(F1, &walk(40, 8)));
    let fit = mk_fit(R1, &[(R1, 0)], &[&t, &f1], &[0.001, 0.5], 0.01, 20);
    let rep = run(&fit, &t, &[f1.clone()]);
    let a = attribute(&fit, &rep, &AttrWindow::Obs { obs: 2, to: None });
    assert!(a.unavailable.is_none());
    close(a.actual, (0.99f64).ln(), 1e-12);
    close(a.simple_actual.unwrap(), -0.01, 1e-12);
    assert_eq!(a.display_unit, "log bp");
    assert_eq!(a.display_scale, 1e4);
    close(a.actual * 1e4, -100.5034, 1e-6);
    // Σ simple returns = 0 ≠ A.
    close((1.1f64 - 1.) + (0.9 - 1.1) / 1.1 * 0. + (99. / 110. - 1.), 0.1 - 0.1, 1e-12);
    assert!(a.actual.abs() > 1e-3);
    // Each contribution is β·d in log units; no expm1 anywhere.
    close(a.intercept, 0.001 * 2., 1e-15);
    close(a.factors[0].contribution, 0.5 * a.factors[0].exposure, 1e-15);
    close(a.actual, a.intercept + a.factors[0].contribution + a.residual, 1e-12);
}

#[test]
fn attribution_never_refits() {
    let (fit, t, f) = v5();
    let rep = run(&fit, &t, &f);
    let (lt, lf) = level_pair(40);
    let lfit = mk_fit(Transform::Level, &[(Transform::Level, 0)], &[&lt, &lf], &[1., 2.], 2., 15);
    let lrep = run(&lfit, &lt, &[lf.clone()]);
    let before = FIT_CALLS.with(|c| c.get());
    for w in [
        AttrWindow::default(),
        AttrWindow::Obs { obs: 5, to: None },
        AttrWindow::Range { from: ts_of(30), to: Some(ts_of(36)) },
        AttrWindow::Obs { obs: 500, to: None },
    ] {
        attribute(&fit, &rep, &w);
        attribute(&lfit, &lrep, &w);
    }
    assert_eq!(FIT_CALLS.with(|c| c.get()), before, "attribution must never call fit_ols");
}

#[test]
fn attribution_gap_window_unavailable() {
    let (t, f1, f2) = (named(TGT, &walk(40, 1)), named(F1, &walk(40, 2)), named(F2, &walk(40, 3)));
    let fit = mk_fit(C1, &[(C1, 0), (C1, 0)], &[&t, &f1, &f2], &[0.1, 0.2, -0.3], 1., 20);
    let gapped = vec![drop_day(f1.clone(), 10), f2.clone()];
    let rep = run(&fit, &t, &gapped);
    assert!(rep.unavailable.is_none());
    let missing: Vec<usize> = (0..40).filter(|i| rep.residual.values[*i].is_none() && *i > 1).collect();
    assert!(!missing.is_empty());
    // A window across the gap is unavailable and names the first (newest) missing row.
    let w = AttrWindow::Obs { obs: 5, to: Some(ts_of(12)) };
    let a = attribute(&fit, &rep, &w);
    let first = *missing.iter().rev().find(|i| **i <= 12).unwrap();
    let why = a.unavailable.clone().expect("gap window must be unavailable");
    assert_eq!(why, format!("window incomplete: no complete model row at {}", stamp(ts_of(first))));
    assert!(a.factors.is_empty() && a.m == 0);
    // A window entirely after the gap works and keeps its nominal m.
    let ok = attribute(&fit, &rep, &AttrWindow::Obs { obs: 5, to: None });
    assert!(ok.unavailable.is_none());
    assert_eq!(ok.m, 5);
    // Nominal m, never "the rows we found": obs larger than the clean run is unavailable.
    let long = attribute(&fit, &rep, &AttrWindow::Obs { obs: 30, to: None });
    assert!(long.unavailable.is_some());
}

#[test]
fn attribution_horizon_stride_and_multiple() {
    let n = 60;
    let (t, f1, f2) = (named(TGT, &walk(n, 21)), named(F1, &walk(n, 22)), named(F2, &walk(n, 23)));
    let h5 = Transform::LogReturn { h: 5 };
    let fit = mk_fit(h5, &[(h5, 0), (R1, 0)], &[&t, &f1, &f2], &[0.001, 0.5, 0.3], 0.01, 30);
    let rep = run(&fit, &t, &[f1.clone(), f2.clone()]);
    assert!(rep.unavailable.is_none(), "{:?}", rep.unavailable);
    let a = attribute(&fit, &rep, &AttrWindow::Obs { obs: 2, to: None });
    assert!(a.unavailable.is_none(), "{:?}", a.unavailable);
    assert_eq!(a.mode, AttrMode::Sum { h: 5 });
    let closes: Vec<f64> = t.bars.iter().map(|b| b.close).collect();
    let b_idx = 59;
    close(a.actual, (closes[b_idx] / closes[b_idx - 10]).ln(), 1e-12);
    assert_eq!(a.from_ts, ts_of(b_idx - 10));
    assert_eq!(a.m, 2);
    // d₀ = m and the drift term is β₀·m.
    close(a.intercept, 0.002, 1e-15);
    // Same-horizon factor telescopes; the h=1 factor in an h=5 target is NotAMove with the exact text.
    assert_eq!(a.factors[0].kind, ExposureKind::Move { from_ts: ts_of(b_idx - 10), to_ts: ts_of(b_idx) });
    assert_eq!(
        a.factors[1].kind,
        ExposureKind::NotAMove { why: "Σ of m overlapping/strided 1-bar values; h_j ≠ h_y".into() }
    );
    let f1c: Vec<f64> = f1.bars.iter().map(|b| b.close).collect();
    close(a.factors[0].exposure, (f1c[b_idx] / f1c[b_idx - 10]).ln(), 1e-12);
    // Identity.
    close(a.actual, a.intercept + a.factors.iter().map(|f| f.contribution).sum::<f64>() + a.residual, 1e-12);
    // `from` seven bars back is not a multiple of h = 5.
    let bad = attribute(&fit, &rep, &AttrWindow::Range { from: ts_of(b_idx - 7), to: None });
    assert_eq!(
        bad.unavailable.unwrap(),
        "window length 7 bars is not a multiple of h=5; overlapping h-bar returns cannot be summed (try obs=1)"
    );
    // A multiple is fine.
    let good = attribute(&fit, &rep, &AttrWindow::Range { from: ts_of(b_idx - 10), to: None });
    assert_eq!(good.m, 2);
    assert_eq!(good.actual, a.actual);
}

#[test]
fn attribution_reconciles_with_scored_residuals() {
    let n = 200;
    let (t, f1, f2, f3) =
        (named(TGT, &walk(n, 31)), named(F1, &walk(n, 32)), named(F2, &walk(n, 33)), named(F3, &walk(n, 34)));
    let fit = mk_fit(R1, &[(R1, 0), (C1, 1), (R1, 2)], &[&t, &f1, &f2, &f3], &[0.0002, 0.7, 0.01, -0.3], 0.01, 100);
    let rep = run(&fit, &t, &[f1.clone(), f2.clone(), f3.clone()]);
    for obs in [1usize, 7, 20, 90] {
        let a = attribute(&fit, &rep, &AttrWindow::Obs { obs, to: None });
        assert!(a.unavailable.is_none(), "{:?}", a.unavailable);
        let sum_c = a.intercept + a.factor_total();
        let tol = 1e-12 * a.actual.abs().max(1.);
        assert!((a.actual - sum_c - a.residual).abs() <= tol);
        assert!((a.residual - a.residual_scored_sum).abs() <= tol);
        let b = 199usize;
        let sum_fit: f64 = (0..obs).map(|i| rep.fitted[b - i].unwrap()).sum();
        assert!((a.predicted - sum_fit).abs() <= tol, "{} vs {sum_fit}", a.predicted);
        assert_eq!(a.path.len(), obs);
        let last = a.path.last().unwrap();
        assert!((last.actual - a.actual).abs() <= tol);
        assert!((last.residual - a.residual).abs() <= tol);
        let sum_path: f64 = last.drift + last.factors.iter().sum::<f64>() + last.residual;
        assert!((sum_path - last.actual).abs() <= tol);
    }
}

#[test]
fn attribution_window_labels_in_out_mixed() {
    let n = 80;
    let (t, f1) = (named(TGT, &walk(n, 41)), named(F1, &walk(n, 42)));
    let h5 = Transform::LogReturn { h: 5 };
    let fit = mk_fit(h5, &[(h5, 0)], &[&t, &f1], &[0., 0.5], 0.01, 30);
    let rep = run(&fit, &t, &[f1.clone()]);
    let at = |obs, day: usize| attribute(&fit, &rep, &AttrWindow::Obs { obs, to: Some(ts_of(day)) });
    assert_eq!(at(1, 20).label, WindowLabel::InSample);
    assert_eq!(at(1, 79).label, WindowLabel::OutOfSample);
    // Rows 36 (SCORED), 31 (EMBARGO: shares training support), 26 (TRAIN).
    assert_eq!(at(3, 36).label, WindowLabel::Mixed { train: 1, embargo: 1, scored: 1 });
    assert_eq!(at(1, 33).label, WindowLabel::Mixed { train: 0, embargo: 1, scored: 0 });
    assert_eq!(at(1, 79).embargo_bars, 4);
    assert_eq!(at(1, 79).trained_thru, ts_of(30));
}

#[test]
fn attribution_rho_hat_respects_breaks() {
    let e = [1., -1., 1., -1.];
    let (r1, p1) = rho_hat(&e, &[false; 4], 1).unwrap();
    close(r1, -0.75, 1e-12);
    assert_eq!(p1, 3);
    let (r2, p2) = rho_hat(&e, &[false; 4], 2).unwrap();
    close(r2, 0.5, 1e-12);
    assert_eq!(p2, 2);
    let brk = [true, false, true, false];
    let (b1, q1) = rho_hat(&e, &brk, 1).unwrap();
    close(b1, -0.5, 1e-12);
    assert_eq!(q1, 2);
    assert!(rho_hat(&e, &brk, 2).is_none());
    assert!(rho_hat(&e, &[false; 4], 4).is_none());
    assert!(rho_hat(&[2., 2., 2.], &[false; 3], 1).is_none());
}

#[test]
fn attribution_residual_variance_bartlett_4p5() {
    // V10
    let (v, fb) = residual_variance_sum(3, 1., &[0.5, 0.25]);
    close(v, 3. + 2. * (2. * (2. / 3.) * 0.5 + 1. * (1. / 3.) * 0.25), 1e-12);
    close(v, 4.5, 1e-12);
    assert!(!fb);
    // V11
    let (v, fb) = residual_variance_sum(2, 1., &[-1.]);
    close(v, 1., 1e-12);
    assert!(!fb);
    let (v, fb) = residual_variance_sum(2, 1., &[-2.5]);
    close(v, 2., 1e-12);
    assert!(fb);
    // V12
    assert_eq!(automatic_bandwidth(5000), 9);
    assert_eq!(automatic_bandwidth(120), 4);
    // Endpoint LOW N: obs=100 leaves 20 residual pairs (< 30): predictive withheld, coefficient kept.
    let (t, f) = level_pair(130);
    let mut fit = mk_fit(Transform::Level, &[(Transform::Level, 0)], &[&t, &f], &[1., 2.], 2., 15);
    fit.coef_covariance = four(&ident(2, 0.2));
    refresh(&mut fit);
    let rep = run(&fit, &t, &[f.clone()]);
    let a = attribute(&fit, &rep, &AttrWindow::Obs { obs: 100, to: None });
    assert!(a.unavailable.is_none(), "{:?}", a.unavailable);
    assert!(a.residual_var.variance.is_none());
    assert!(a.residual_var.unavailable.as_deref().unwrap().starts_with("LOW N: 20 stored residual pairs at lag 100 < 30"));
    for c in &a.covariances {
        assert!(c.coefficient_se.is_some() && c.predictive_se.is_none());
    }
    // A short lag has enough pairs.
    let b = attribute(&fit, &rep, &AttrWindow::Obs { obs: 3, to: None });
    assert!(b.residual_var.variance.is_some());
    assert!(b.covariances[0].predictive_se.is_some());
}

#[test]
fn attribution_key_excludes_wall_clock_and_tracks_inputs() {
    let (fit, t, f) = v5();
    let w = AttrWindow::Obs { obs: 2, to: None };
    let scored = |t: &NamedBars, f: &[NamedBars], observed: Option<i64>| {
        let key = score_key(&fit, t, f, "unit", true, 0).params;
        score(&fit, t, f, true, "unit", 0, observed, key)
    };
    let r1 = scored(&t, &f, Some(1));
    let r2 = scored(&t, &f, Some(2));
    assert_eq!(attribution_key(&fit, &r1, &w), attribution_key(&fit, &r2, &w));
    assert_eq!(attribute(&fit, &r1, &w).key, attribute(&fit, &r2, &w).key);
    let base = attribution_key(&fit, &r1, &w);
    // A corrected old close changes the scored report's key and so the attribution's.
    let mut t2 = t.clone();
    t2.bars[3].close += 0.5;
    assert_ne!(attribution_key(&fit, &scored(&t2, &f, Some(1)), &w), base);
    // Window.
    assert_ne!(attribution_key(&fit, &r1, &AttrWindow::Obs { obs: 3, to: None }), base);
    assert_ne!(attribution_key(&fit, &r1, &AttrWindow::Obs { obs: 2, to: Some(ts_of(38)) }), base);
    assert_ne!(
        attribution_key(&fit, &r1, &AttrWindow::Range { from: ts_of(37), to: None }),
        attribution_key(&fit, &r1, &AttrWindow::Range { from: ts_of(37), to: Some(0) })
    );
    // A gap.
    let gapped = scored(&t, &[drop_day(f[0].clone(), 5), f[1].clone()], Some(1));
    assert_ne!(attribution_key(&fit, &gapped, &w), base);
    // Model digest.
    let mut fit2 = fit.clone();
    fit2.coef_covariance = four(&ident(3, 0.2));
    refresh(&mut fit2);
    assert_ne!(attribution_key(&fit2, &r1, &w), base);
}

#[test]
fn attribution_append_invariant() {
    let long = |n: usize| {
        let (t, f1, f2) = (named(TGT, &walk(n, 51)), named(F1, &walk(n, 52)), named(F2, &walk(n, 53)));
        (t, vec![f1, f2])
    };
    let (t40, f40) = long(40);
    let mut fit = mk_fit(C1, &[(C1, 0), (C1, 0)], &[&t40, &f40[0], &f40[1]], &[0.1, 0.2, -0.3], 1., 15);
    fit.coef_covariance = four(&ident(3, 0.05));
    refresh(&mut fit);
    let w = AttrWindow::Range { from: ts_of(20), to: Some(ts_of(30)) };
    let a = attribute(&fit, &run(&fit, &t40, &f40), &w);
    assert!(a.unavailable.is_none(), "{:?}", a.unavailable);
    let (t45, f45) = long(45);
    let b = attribute(&fit, &run(&fit, &t45, &f45), &w);
    assert_eq!(a.actual.to_bits(), b.actual.to_bits());
    assert_eq!(a.intercept.to_bits(), b.intercept.to_bits());
    assert_eq!(a.residual.to_bits(), b.residual.to_bits());
    for (x, y) in a.factors.iter().zip(&b.factors) {
        assert_eq!(x.contribution.to_bits(), y.contribution.to_bits());
        assert_eq!(x.coefficient_se, y.coefficient_se);
    }
    assert_eq!(a.covariances, b.covariances);
    assert_eq!(a.path, b.path);
}

#[test]
fn attribution_kind_flags_keep_identity() {
    // A LogLevel factor in a return model.
    let (t, f1) = (named(TGT, &walk(50, 61)), named(F1, &walk(50, 62)));
    let fit = mk_fit(R1, &[(Transform::LogLevel, 0)], &[&t, &f1], &[0.001, 0.1], 0.01, 20);
    let rep = run(&fit, &t, &[f1.clone()]);
    let a = attribute(&fit, &rep, &AttrWindow::Obs { obs: 5, to: None });
    assert!(a.unavailable.is_none(), "{:?}", a.unavailable);
    assert_eq!(
        a.factors[0].kind,
        ExposureKind::NotAMove { why: "Σ of m levels = m × mean level; not a move".into() }
    );
    close(a.actual, a.intercept + a.factors[0].contribution + a.residual, 1e-12);
    // A return factor in a level model.
    let (lt, lf) = level_pair(50);
    let lfit = mk_fit(Transform::Level, &[(R1, 0)], &[&lt, &lf], &[1., 2.], 2., 20);
    let lrep = run(&lfit, &lt, &[lf.clone()]);
    let b = attribute(&lfit, &lrep, &AttrWindow::Obs { obs: 3, to: None });
    assert!(b.unavailable.is_none(), "{:?}", b.unavailable);
    assert_eq!(
        b.factors[0].kind,
        ExposureKind::NotAMove { why: "difference of two 1-bar values; not a move".into() }
    );
    close(b.actual, b.predicted + b.residual, 1e-12);
    close(b.residual, b.residual_scored_sum, 1e-12);
}

#[test]
fn attr_args_grammar() {
    let ok = |s: &str| AttrArgs::parse(s).unwrap();
    assert_eq!(ok(""), AttrArgs::default());
    assert_eq!(ok("model=AAPL-v-SPY-v1").model.as_deref(), Some("AAPL-v-SPY-v1"));
    assert_eq!(ok("obs=20").window, Some(AttrWindow::Obs { obs: 20, to: None }));
    let day = |d: &str| parse_time(d).unwrap();
    assert_eq!(
        ok("MODEL=AAPL-v-SPY-v1 from=2026-03-02 to=2026-03-20").window,
        Some(AttrWindow::Range { from: day("2026-03-02"), to: Some(day("2026-03-20")) })
    );
    assert_eq!(day("2026-03-02") % 86_400, 86_399);
    assert_eq!(
        ok("from=2026-03-02T14:30 to=2026-03-02T20:00Z").window,
        Some(AttrWindow::Range {
            from: chrono::NaiveDate::from_ymd_opt(2026, 3, 2).unwrap().and_hms_opt(14, 30, 0).unwrap().and_utc().timestamp(),
            to: Some(
                chrono::NaiveDate::from_ymd_opt(2026, 3, 2).unwrap().and_hms_opt(20, 0, 0).unwrap().and_utc().timestamp()
            ),
        })
    );
    assert_eq!(ok("to=2026-03-20").window, Some(AttrWindow::Obs { obs: 1, to: Some(day("2026-03-20")) }));
    for bad in [
        "obs=0",
        "obs=1001",
        "obs=5 from=2026-03-02",
        "obs=5 obs=6",
        "model=A-v1 model=B-v1",
        "from=2026-13-01",
        "model=../x",
        "bogus",
        "foo=1",
        "from=2026-03-20 to=2026-03-02",
    ] {
        assert!(AttrArgs::parse(bad).is_err(), "{bad}");
    }
    // Round trip through `words`.
    for w in [
        AttrWindow::Obs { obs: 20, to: None },
        AttrWindow::Range { from: day("2026-03-02"), to: Some(day("2026-03-20")) },
        AttrWindow::Obs { obs: 3, to: Some(day("2026-03-20")) },
    ] {
        assert_eq!(AttrArgs::parse(&w.words(true)).unwrap().window, Some(w));
    }
}

#[test]
fn attribution_refuses_bad_reports() {
    let (fit, t, f) = v5();
    let mut rep = run(&fit, &t, &f);
    let mut other = fit.clone();
    other.coef_covariance = four(&ident(3, 1.));
    refresh(&mut other);
    let a = attribute(&other, &rep, &AttrWindow::default());
    assert!(a.unavailable.unwrap().contains("different model"));
    rep.provenance = Provenance::Sample;
    assert!(attribute(&fit, &rep, &AttrWindow::default()).unavailable.is_some());
    rep.provenance = Provenance::Real;
    rep.unavailable = Some("nope".into());
    assert!(attribute(&fit, &rep, &AttrWindow::default()).unavailable.unwrap().contains("nope"));
    // A window before the scored grid is unavailable with the first bar named.
    let ok = run(&fit, &t, &f);
    let early = attribute(&fit, &ok, &AttrWindow::Range { from: T0 - DAY, to: None });
    assert!(early.unavailable.unwrap().contains("before the scored grid"));
    // Models saved before slice 7 carry no covariance: SEs are simply absent.
    let none = attribute(&fit, &ok, &AttrWindow::default());
    assert!(none.covariances.is_empty() && none.default_cov.is_none());
}

#[test]
fn attr_window_words_round_trip() {
    for daily in [true, false] {
        for w in [
            AttrWindow::Range { from: T0 + 4_980, to: Some(T0 + 86_399) },
            AttrWindow::Range { from: T0 + 86_399, to: None },
            AttrWindow::Obs { obs: 5, to: Some(T0 + 43_200) },
            AttrWindow::Obs { obs: 3, to: Some(T0 + 172_799) },
        ] {
            let words = w.words(daily);
            let parsed = AttrArgs::parse(&words).unwrap().window.unwrap();
            assert_eq!(parsed, w, "{words}");
        }
    }
}
