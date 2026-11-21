//! Known answers, strict-grid admission, purge, frozen-holdout and parser tests for the Lab.
use super::*;
use crate::studio_corr::{Options, prepare};
use crate::studio_inference::{automatic_bandwidth, regression_covariance};

const DAY: i64 = 86_400;
const T0: i64 = 1_699_920_000;
const TGT: &str = "TTT Curncy";
const F1: &str = "FFF Curncy";
const F2: &str = "GGG Curncy";

fn lcg(s: &mut u64) -> f64 {
    *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*s >> 11) as f64 / (1u64 << 53) as f64
}
fn bar(ts: i64, c: f64) -> Bar {
    Bar::new(ts, c, c, c, c, 1.).with_provenance(Provenance::Real)
}
fn named(symbol: &str, days: &[i64], closes: &[f64]) -> NamedBars {
    NamedBars {
        symbol: symbol.into(),
        bars: days.iter().zip(closes).map(|(d, c)| bar(T0 + d * DAY, *c)).collect(),
        source: "unit".into(),
        ..Default::default()
    }
}
fn walk(n: usize, seed: u64) -> Vec<f64> {
    let mut s = seed;
    let mut p = 100.;
    (0..n)
        .map(|_| {
            p *= (0.02 * (lcg(&mut s) - 0.5)).exp();
            p
        })
        .collect()
}
/// Target log returns = β·factor log returns + noise, so a one-factor fit has a real slope.
fn related(n: usize, beta: f64, seed: u64) -> (Vec<f64>, Vec<f64>) {
    let f = walk(n, seed);
    let mut s = seed ^ 99;
    let mut p = 50.;
    let t = (0..n)
        .map(|i| {
            if i > 0 {
                p *= (beta * (f[i] / f[i - 1]).ln() + 0.004 * (lcg(&mut s) - 0.5)).exp();
            }
            p
        })
        .collect();
    (t, f)
}
fn days(n: usize) -> Vec<i64> {
    (0..n as i64).collect()
}
fn rel_rows(n: usize, beta: f64, seed: u64) -> (NamedBars, NamedBars) {
    let (t, f) = related(n, beta, seed);
    (named(TGT, &days(n), &t), named(F1, &days(n), &f))
}
fn spec1(target: Transform, f: Transform, lag: usize) -> LabSpec {
    LabSpec { target, factors: vec![FactorSpec::new(F1, f, lag)], ..Default::default() }
}
fn design(t: &NamedBars, f: &[NamedBars], s: &LabSpec) -> Design {
    build_design(t, f, s, Timeframe::D1, true, "unit", 0, None).unwrap()
}
fn near(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol * b.abs().max(1.), "{a} != {b}");
}
fn block32() -> (Vec<Vec<f64>>, Vec<f64>, Vec<bool>) {
    let x1: Vec<f64> = [-1., -1., 1., 1.].repeat(8);
    let x2: Vec<f64> = [-1., 1., -1., 1.].repeat(8);
    let rows = x1.iter().zip(&x2).map(|(a, b)| vec![1., *a, *b]).collect();
    let e = [1., -1., -1., 1.].repeat(8);
    let mut b = vec![false; 32];
    b[0] = true;
    (rows, e, b)
}

#[test]
fn lab_classical_hc3_hand_vector_n32() {
    let (x, e, b) = block32();
    let c = regression_covariance(&x, &e, &b, CovarianceMethod::Classical).covariance.unwrap();
    let h = regression_covariance(&x, &e, &b, CovarianceMethod::HC3).covariance.unwrap();
    for i in 0..3 {
        near(c[i][i], 1. / 29., 1e-12);
        near(h[i][i], 32. / 841., 1e-12);
        for j in 0..3 {
            if i != j {
                assert!(c[i][j].abs() < 1e-14 && h[i][j].abs() < 1e-14, "off-diagonal {i},{j}");
            }
        }
    }
}

#[test]
fn lab_hac_auto_hand_vector_n32() {
    let (x, e, b) = block32();
    assert_eq!(automatic_bandwidth(32), 3);
    let r = regression_covariance(&x, &e, &b, CovarianceMethod::HacAutomatic);
    assert_eq!(r.lag, 3);
    let v = r.covariance.unwrap();
    let hand = [[1., 0., 0.], [0., 1., 1.], [0., 1., 3.]];
    // Direct reference: (X'X)^-1 S (X'X)^-1 with X'X = 32 I.
    let mut s = [[0.; 3]; 3];
    let score = |t: usize, i: usize| x[t][i] * e[t];
    for t in 0..32 {
        for i in 0..3 {
            for j in 0..3 {
                s[i][j] += score(t, i) * score(t, j);
            }
        }
    }
    for l in 1..=3usize {
        let w = 1. - l as f64 / 4.;
        for t in l..32 {
            for i in 0..3 {
                for j in 0..3 {
                    s[i][j] += w * (score(t, i) * score(t - l, j) + score(t - l, i) * score(t, j));
                }
            }
        }
    }
    for i in 0..3 {
        for j in 0..3 {
            near(v[i][j], s[i][j] / 1024., 1e-12);
            near(v[i][j], hand[i][j] / 1024., 1e-12);
        }
    }
}

#[test]
fn lab_covariance_low_n_below_30_unavailable() {
    let x1: Vec<f64> = [-1., -1., 1., 1.].repeat(2);
    let x2: Vec<f64> = [-1., 1., -1., 1.].repeat(2);
    let x: Vec<Vec<f64>> = x1.iter().zip(&x2).map(|(a, b)| vec![1., *a, *b]).collect();
    let e = [1., -1., -1., 1.].repeat(2);
    for m in SE_METHODS {
        let r = regression_covariance(&x, &e, &[false; 8], m);
        assert!(r.covariance.is_none() && r.unavailable.as_deref().unwrap().contains("LOW N"), "{m:?}");
    }
}

#[test]
fn lab_covariance_unscale_matches_raw() {
    // Deterministic n=120 design: columns at very different scales, one break at row 60.
    let n = 120;
    let mut s = 5;
    let x1: Vec<f64> = (0..n).map(|_| 1e-4 * (lcg(&mut s) + 0.3)).collect();
    let x2: Vec<f64> = (0..n).map(|i| 1e3 * (lcg(&mut s) - 0.2 + 0.001 * i as f64)).collect();
    let y: Vec<f64> = (0..n).map(|i| 2. + 3e3 * x1[i] - 4e-4 * x2[i] + 0.1 * (lcg(&mut s) - 0.5)).collect();
    let mut breaks = vec![false; n];
    breaks[0] = true;
    breaks[60] = true;
    let fit = linalg::fit_ols(&[&x1, &x2], &y).unwrap();
    let raw: Vec<Vec<f64>> = (0..n).map(|i| vec![1., x1[i], x2[i]]).collect();
    let z = linalg::standardized_rows(&fit, &[&x1, &x2]);
    for m in SE_METHODS {
        let a = regression_covariance(&raw, &fit.residuals, &breaks, m).covariance.unwrap();
        let mut zc = regression_covariance(&z, &fit.residuals, &breaks, m);
        let b = linalg::unscale_covariance(zc.covariance.as_ref().unwrap(), &fit.mean, &fit.scale);
        zc.covariance = Some(b.clone());
        for i in 0..3 {
            for j in 0..3 {
                let scale = (a[i][i] * a[j][j]).sqrt();
                assert!((a[i][j] - b[i][j]).abs() <= 1e-9 * scale, "{m:?} {i},{j}: {} vs {}", a[i][j], b[i][j]);
            }
        }
    }
}

#[test]
fn lab_factor_lag_never_leads() {
    // Factor close = 100 + day index. Target has a hole at day 30 → a break before target bar 30.
    let n = 60;
    let tdays: Vec<i64> = (0..n as i64).filter(|d| *d != 30).collect();
    let tc: Vec<f64> = tdays.iter().map(|d| 10. + *d as f64).collect();
    let target = named(TGT, &tdays, &tc);
    let fd = days(n);
    let fc: Vec<f64> = fd.iter().map(|d| 100. + *d as f64).collect();
    let factor = named(F1, &fd, &fc);
    let lagged = design(&target, &[factor.clone()], &spec1(Transform::Level, Transform::Level, 3));
    for i in 0..lagged.y.len() {
        let t = lagged.bar[i];
        assert!(t >= 3);
        let s_day = (target.bars[t - 3].ts - T0) / DAY;
        assert_eq!(lagged.x[0][i], 100. + s_day as f64, "row {i}");
        // Never a later bar than the row's own.
        assert!(s_day <= (lagged.ts[i] - T0) / DAY);
    }
    // Target bars 30..=32 reach back across the break: x is missing, never bar t−4.
    for t in [30, 31, 32] {
        assert!(!lagged.bar.contains(&t), "bar {t} must be dropped");
    }
    assert!(lagged.bar.contains(&33));
    assert_eq!(lagged.dropped.iter().find(|(r, _)| r == "FFF lag crosses break").map(|d| d.1), Some(3));
    let now = design(&target, &[factor], &spec1(Transform::Level, Transform::Level, 0));
    assert_eq!(now.y.len(), tdays.len());
    for i in 0..now.y.len() {
        assert_eq!(now.x[0][i], 100. + ((now.ts[i] - T0) / DAY) as f64);
    }
}

#[test]
fn lab_design_prefix_invariant() {
    let n = 160;
    let (t, f) = related(n + 50, 0.6, 21);
    let g = walk(n + 50, 8);
    let spec = LabSpec {
        target: Transform::LogReturn { h: 5 },
        factors: vec![
            FactorSpec::new(F1, Transform::LogReturn { h: 1 }, 2),
            FactorSpec::new(F2, Transform::Change { h: 5 }, 0),
        ],
        ..Default::default()
    };
    let mk = |m: usize| {
        let d = days(m);
        (named(TGT, &d, &t[..m]), vec![named(F1, &d, &f[..m]), named(F2, &d, &g[..m])])
    };
    let (ta, fa) = mk(n);
    let (tb, fb) = mk(n + 50);
    let (a, b) = (design(&ta, &fa, &spec), design(&tb, &fb, &spec));
    assert!(a.y.len() > 100 && b.y.len() == a.y.len() + 50);
    let bits = |v: &[f64]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    assert_eq!(a.ts[..], b.ts[..a.y.len()]);
    assert_eq!(bits(&a.y), bits(&b.y[..a.y.len()]));
    for j in 0..2 {
        assert_eq!(bits(&a.x[j]), bits(&b.x[j][..a.y.len()]));
    }
    assert_eq!(a.break_before[..], b.break_before[..a.y.len()]);
}

#[test]
fn lab_single_factor_matches_relate_prepare() {
    let n = 120;
    let hole: Vec<i64> = (0..n as i64).filter(|d| *d != 40 && *d != 77).collect();
    let (t, f) = related(n, 0.7, 4);
    let pick = |v: &[f64]| hole.iter().map(|d| v[*d as usize]).collect::<Vec<_>>();
    let (target, factor) = (named(TGT, &hole, &pick(&t)), named(F1, &hole, &pick(&f)));
    for tr in [Transform::Level, Transform::LogLevel, Transform::Change { h: 5 }, Transform::LogReturn { h: 5 }, Transform::LogReturn { h: 1 }] {
        let p = prepare(&target, &factor, Options { annual_periods: 365., daily: true }, Timeframe::D1, tr, "unit", 0, None).unwrap();
        let rows = p.valid_rows();
        let d = design(&target, &[factor.clone()], &spec1(tr, tr, 0));
        assert_eq!(d.bar, rows, "{tr:?}");
        for (i, &r) in rows.iter().enumerate() {
            assert_eq!(d.y[i].to_bits(), p.y.values[r].unwrap().to_bits(), "{tr:?} y {i}");
            assert_eq!(d.x[0][i].to_bits(), p.x.values[r].unwrap().to_bits(), "{tr:?} x {i}");
        }
    }
}

#[test]
fn lab_purge_removes_cross_split_targets() {
    let (t, f) = rel_rows(205, 0.5, 3);
    let spec = spec1(Transform::LogReturn { h: 5 }, Transform::LogReturn { h: 1 }, 0);
    let d = design(&t, &[f], &spec);
    assert_eq!((d.y.len(), d.bar[0], *d.bar.last().unwrap()), (200, 5, 204));
    let (train, purged, test, split_ts) = split(&d, 20, 5).unwrap();
    assert_eq!((train.len(), purged, test.len()), (156, 4, 40));
    assert_eq!((d.bar[train[0]], d.bar[*train.last().unwrap()]), (5, 160));
    assert_eq!((d.bar[test[0]], *d.bar.last().unwrap()), (165, 204));
    assert_eq!(split_ts, d.ts[test[0]]);
    // Training target supports (g−5, g] never meet any test support (s−5, s].
    for &i in &train {
        for &j in &test {
            assert!(d.bar[i] + 5 <= d.bar[j], "support overlap {i}/{j}");
        }
    }
    // h_y = 1 purges nothing.
    assert_eq!(split(&d, 20, 1).unwrap().1, 0);
    assert!(split(&d, 20, 1).unwrap().0.len() == 160);
}

#[test]
fn lab_holdout_28_over_29() {
    let (a, b, ybar) = (1., 2., 1.);
    let x = [1., 2.];
    let y = [3., 6.];
    let yhat: Vec<f64> = x.iter().map(|x| a + b * x).collect();
    assert_eq!(yhat, [3., 5.]);
    near(oos_r2(&y, &yhat, ybar).unwrap(), 28. / 29., 1e-15);
    assert_eq!(oos_r2(&[2., 2.], &[1., 1.], 2.), None);
}

#[test]
fn lab_holdout_frozen_against_test_perturbation() {
    let n = 260;
    let (t, f) = related(n, 0.8, 12);
    let spec = spec1(Transform::LogReturn { h: 1 }, Transform::LogReturn { h: 1 }, 0);
    let run = |t: &[f64]| {
        let (target, factor) = (named(TGT, &days(n), t), named(F1, &days(n), &f));
        compute(&target, &[factor], &spec, Timeframe::D1, true, "unit", 0, None, 1)
    };
    let a = run(&t);
    let mut t2 = t.clone();
    t2[n - 3] *= 1.07; // inside the test span
    let b = run(&t2);
    let (fa, fb) = (a.fit.as_ref().unwrap(), b.fit.as_ref().unwrap());
    assert_eq!(fa.coefficients, fb.coefficients);
    assert_eq!((&fa.mean, &fa.scale, fa.train_mean), (&fb.mean, &fb.scale, fb.train_mean));
    assert_eq!((fa.n_train, fa.train_end, fa.split_ts), (fb.n_train, fb.train_end, fb.split_ts));
    assert_ne!(a.holdout.as_ref().unwrap().r2_oos, b.holdout.as_ref().unwrap().r2_oos);
    assert!(a.holdout.unwrap().r2_oos.is_some());
}

#[test]
fn lab_min_rows_thresholds() {
    let go = |rows: usize, pct: u8| {
        let (t, f) = related(rows + 1, 0.8, 31);
        let (target, factor) = (named(TGT, &days(rows + 1), &t), named(F1, &days(rows + 1), &f));
        let mut spec = spec1(Transform::LogReturn { h: 1 }, Transform::LogReturn { h: 1 }, 0);
        spec.holdout_pct = pct;
        compute(&target, &[factor], &spec, Timeframe::D1, true, "unit", 0, None, 1)
    };
    assert_eq!(min_train(2), 60);
    assert_eq!(min_train(8), 80);
    // Training rows at min_train − 1 and min_train (test rows ample).
    let low = go(99, 40);
    assert_eq!(low.fit.as_ref().unwrap().n_train, 59);
    assert!(low.low_n.as_deref().unwrap().contains("training rows 59 < 60"), "{:?}", low.low_n);
    assert!(low.inference.is_empty(), "SE withheld");
    assert!(low.fit.as_ref().unwrap().coefficients.len() == 2 && low.fit.as_ref().unwrap().r2_in.is_some());
    let ok = go(100, 40);
    assert_eq!(ok.fit.as_ref().unwrap().n_train, 60);
    assert!(ok.low_n.is_none() && ok.inference.len() == 4);
    assert!(ok.inference[3].se.iter().all(Option::is_some));
    // Test rows at 19 and 20.
    let few = go(95, 20);
    assert_eq!(few.holdout.as_ref().unwrap().n_test, 19);
    assert!(few.holdout.as_ref().unwrap().r2_oos.is_none() && few.low_n.is_some());
    assert_eq!(few.inference.len(), 4);
    let enough = go(100, 20);
    assert_eq!(enough.holdout.as_ref().unwrap().n_test, 20);
    assert!(enough.holdout.as_ref().unwrap().r2_oos.is_some() && enough.low_n.is_none());
}

#[test]
fn lab_missing_factor_not_dropped() {
    let n = 200;
    let (t, f) = related(n, 0.8, 6);
    let g = walk(n, 9);
    let target = named(TGT, &days(n), &t);
    let spec = LabSpec {
        factors: vec![
            FactorSpec::new(F1, Transform::LogReturn { h: 1 }, 0),
            FactorSpec::new(F2, Transform::LogReturn { h: 1 }, 0),
        ],
        ..Default::default()
    };
    let good = named(F1, &days(n), &f);
    let run = |g2: NamedBars| compute(&target, &[good.clone(), g2], &spec, Timeframe::D1, true, "unit", 0, None, 1);
    let ok = run(named(F2, &days(n), &g));
    assert!(ok.fit.is_some() && ok.factor_errors.is_empty() && ok.unavailable.is_none());
    // An error row.
    let mut err = named(F2, &days(n), &g);
    err.error = Some("no archive".into());
    // Unverified provenance.
    let mut unverified = named(F2, &days(n), &g);
    unverified.bars.iter_mut().for_each(|b| b.provenance = Provenance::Sample);
    // An invalid bar early, outside the final complete-case intersection (after the first rows are
    // consumed by the transform and lag): still refused on the full series.
    let mut bad = named(F2, &days(n), &g);
    bad.bars[0].close = f64::NAN;
    let mut negative = named(F2, &days(n), &g);
    negative.bars[1].close = -3.;
    for (label, row) in [("error", err), ("unverified", unverified), ("nan", bad), ("negative", negative)] {
        let r = run(row);
        assert!(r.fit.is_none() && r.inference.is_empty() && r.points.is_empty(), "{label}");
        assert_eq!(r.factor_errors.len(), 1, "{label}");
        assert_eq!(r.factor_errors[0].0, F2);
        assert!(r.unavailable.as_deref().unwrap().contains("No partial fit"), "{label}");
    }
}

#[test]
fn lab_key_changes_on_old_bar_correction() {
    let n = 120;
    let (target, factor) = rel_rows(n, 0.8, 2);
    let g = named(F2, &days(n), &walk(n, 3));
    let spec = LabSpec {
        factors: vec![
            FactorSpec::new(F1, Transform::LogReturn { h: 1 }, 0),
            FactorSpec::new(F2, Transform::LogReturn { h: 1 }, 1),
        ],
        ..Default::default()
    };
    let key = |t: &NamedBars, f: &[NamedBars], s: &LabSpec| cache_key(t, f, s, Timeframe::D1, "src", true, Some(5), 77).params;
    let base = key(&target, &[factor.clone(), g.clone()], &spec);
    assert_eq!(base, key(&target, &[factor.clone(), g.clone()], &spec));
    // A correction inside an old bar of any series.
    let mut t2 = target.clone();
    t2.bars[3].close *= 1.001;
    assert_ne!(base, key(&t2, &[factor.clone(), g.clone()], &spec));
    let mut f2 = factor.clone();
    f2.bars[2].close *= 1.001;
    assert_ne!(base, key(&target, &[f2, g.clone()], &spec));
    // Factor order matters (coefficient order == spec order).
    let mut swapped = spec.clone();
    swapped.factors.reverse();
    assert_ne!(base, key(&target, &[g.clone(), factor.clone()], &swapped));
    // Lag / holdout / transform change the key; the displayed covariance does not.
    let mut lag = spec.clone();
    lag.factors[1].lag = 2;
    assert_ne!(base, key(&target, &[factor.clone(), g.clone()], &lag));
    let mut hold = spec.clone();
    hold.holdout_pct = 30;
    assert_ne!(base, key(&target, &[factor.clone(), g.clone()], &hold));
    let mut cov = spec.clone();
    cov.covariance = CovarianceMethod::Classical;
    assert_eq!(base, key(&target, &[factor.clone(), g.clone()], &cov));
    // The observation clock and the closed-through stamp are keyed.
    let a = cache_key(&target, &[factor.clone(), g.clone()], &spec, Timeframe::D1, "src", true, Some(6), 77).params;
    assert_ne!(base, a);
}

#[test]
fn lab_args_parse_and_round_trip() {
    let a = LabArgs::parse("5m factors=SPY,TLT:change:5:1 target=logreturn:5 covariance=hac5 holdout=25%").unwrap();
    assert_eq!(a.tf, Some(Timeframe::M5));
    let mut spec = LabSpec::default();
    a.apply(&mut spec).unwrap();
    assert_eq!(spec.target, Transform::LogReturn { h: 5 });
    assert_eq!(spec.factors.len(), 2);
    assert_eq!(spec.factors[1], FactorSpec::new("TLT", Transform::Change { h: 5 }, 1));
    assert_eq!((spec.holdout_pct, spec.covariance), (25, CovarianceMethod::HacFixed5));
    let again = {
        let mut s = LabSpec::default();
        LabArgs::parse(&spec.command()).unwrap().apply(&mut s).unwrap();
        s
    };
    assert_eq!(again, spec);
    // Level factor and a crypto key also round-trip.
    let mut odd = spec.clone();
    odd.factors.push(FactorSpec::new("GLD", Transform::LogLevel, 7));
    odd.factors.push(FactorSpec::new(F2, Transform::Level, 0));
    let mut s = LabSpec::default();
    LabArgs::parse(&odd.command()).unwrap().apply(&mut s).unwrap();
    assert_eq!(s, odd);
    // Edit groups.
    let mut e = LabSpec::default_for("AAPL");
    LabArgs::parse("factor=TLT transform=change h=5 lag=1 remove=SPY").unwrap().apply(&mut e).unwrap();
    assert_eq!(e.factors, vec![FactorSpec::new("TLT", Transform::Change { h: 5 }, 1)]);
    let mut e2 = e.clone();
    LabArgs::parse("factor=TLT lag=4").unwrap().apply(&mut e2).unwrap();
    assert_eq!(e2.factors[0].lag, 4);
    assert_eq!(e2.factors[0].transform, Transform::Change { h: 5 });
    // Errors.
    for bad in [
        "factors=SPY:logreturn:3",
        "factors=SPY:logreturn:1:251",
        "factors=A,B,C,D,E,F,G,H",
        "factors=SPY,SPY",
        "transform=change",
        "h=5",
        "lag=1",
        "factor=TLT lag=1 lag=2",
        "factors=",
        "holdout=5%",
        "holdout=50%",
        "target=logreturn:3",
        "covariance=nope",
        "frobnicate=1",
        "1w",
        "5m 1h",
    ] {
        assert!(LabArgs::parse(bad).is_err(), "{bad}");
    }
    assert!(LabArgs::parse("transform=change").unwrap_err().contains("use target=… for the target"));
    assert!(LabArgs::parse("target=logreturn:3").unwrap_err().contains("Lab h must be 1, 5 or 20"));
    // A factor equal to the target, and more than seven factors after edits.
    let mut s = LabSpec::default();
    LabArgs::parse("factors=AAPL").unwrap().apply(&mut s).unwrap();
    assert!(s.validate_for("AAPL US Equity").is_err());
    assert!(s.validate_for("MSFT").is_ok());
    let mut s = LabSpec::default();
    let err = LabArgs::parse("factors=A,B,C,D,E,F,G factor=H").unwrap().apply(&mut s);
    assert!(err.is_err());
}

#[test]
fn lab_end_to_end_recovers_slope_with_inference() {
    let n = 600;
    let (target, factor) = rel_rows(n, 0.8, 55);
    let spec = spec1(Transform::LogReturn { h: 1 }, Transform::LogReturn { h: 1 }, 0);
    let r = compute(&target, &[factor], &spec, Timeframe::D1, true, "unit", 99, Some(5), 7);
    assert!(r.unavailable.is_none() && r.low_n.is_none(), "{:?}", r.low_n);
    let fit = r.fit.as_ref().unwrap();
    assert!((fit.coefficients[1] - 0.8).abs() < 0.05, "{}", fit.coefficients[1]);
    assert_eq!((fit.k, fit.rank, fit.n_train), (2, 2, 479));
    assert!(fit.r2_in.unwrap() > 0.9 && fit.kappa_inf >= 1.);
    assert_eq!(r.inference.len(), 4);
    for inf in &r.inference {
        assert!(inf.se[1].is_some() && inf.p[1].unwrap() < 1e-6 && inf.q[1].unwrap() < 1e-3 && inf.q[0].is_none());
    }
    // All four labels are distinct and HAC lag/kernel are printed.
    let labels: Vec<_> = r.inference.iter().map(|i| i.covariance.label()).collect();
    assert!(labels[2].contains("L=5") && labels[3].contains("automatic"));
    let ho = r.holdout.as_ref().unwrap();
    assert!(ho.r2_oos.unwrap() > 0.8 && ho.forecast_style == false);
    // Bounded diagnostics: every training residual is counted exactly once.
    let dg = r.diagnostics.as_ref().unwrap();
    assert_eq!(dg.hist.iter().sum::<u32>() + dg.hist_beyond, fit.n_train as u32);
    assert_eq!(dg.grid.iter().sum::<u32>() + dg.grid_outside, fit.n_train as u32);
    assert!(r.points.len() <= MAX_POINTS && r.points.last().unwrap().segment == Segment::Test);
    assert!(r.points.iter().filter(|p| p.segment == Segment::Purged).count() == 0);
    assert_eq!(r.closed_thru, 99);
    // A lagged factor on this contemporaneous relationship is not a forecast-style fit and loses fit.
    let (target, factor) = rel_rows(n, 0.8, 55);
    let lagged = compute(&target, &[factor], &spec1(spec.target, spec.factors[0].transform, 1), Timeframe::D1, true, "unit", 99, Some(5), 7);
    assert!(lagged.holdout.unwrap().forecast_style);
}

#[test]
fn lab_and_relate_reports_carry_bounded_training_residuals() {
    // Lab: the stored tail equals the fit_ols residuals; bounded at 5,000; mean over all n_train.
    let n = 6_600;
    let (target, factor) = rel_rows(n, 0.8, 77);
    let spec = spec1(Transform::LogReturn { h: 1 }, Transform::LogReturn { h: 1 }, 0);
    let r = compute(&target, &[factor.clone()], &spec, Timeframe::D1, true, "unit", 0, None, 1);
    let fit = r.fit.as_ref().unwrap();
    assert!(fit.n_train > crate::studio_residual::MAX_TRAIN_RESIDUALS);
    assert_eq!(r.train_residuals.len(), crate::studio_residual::MAX_TRAIN_RESIDUALS);
    assert_eq!(r.train_breaks.len(), r.train_residuals.len());
    assert!(r.train_breaks[0]);
    let d = design(&target, &[factor], &spec);
    let cols: Vec<&[f64]> = d.x.iter().map(|c| &c[..fit.n_train]).collect();
    let ols = linalg::fit_ols(&cols, &d.y[..fit.n_train]).unwrap();
    let tail = &ols.residuals[fit.n_train - r.train_residuals.len()..];
    for (a, b) in r.train_residuals.iter().zip(tail) {
        assert!((a - b).abs() <= 1e-12, "{a} vs {b}");
    }
    let mean = ols.residuals.iter().sum::<f64>() / fit.n_train as f64;
    assert!((r.train_residual_mean - mean).abs() <= 1e-15);
    assert_eq!(r.train_span_digests.len(), fit.k);
    assert_eq!(r.train_span_digests[0].symbol, TGT);
    assert_eq!(r.train_span_digests[0].thru_ts, fit.train_end);
    // Old exports without the new fields still deserialize (empty, so a freeze refuses them).
    let mut json: serde_json::Value = serde_json::to_value(&r).unwrap();
    for k in ["train_residuals", "train_breaks", "train_residual_mean", "train_span_digests"] {
        json.as_object_mut().unwrap().remove(k);
    }
    let old: LabReport = serde_json::from_value(json).unwrap();
    assert!(old.train_residuals.is_empty() && old.train_span_digests.is_empty());
}
