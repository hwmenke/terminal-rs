//! Known answers, strict-convention admission and gap tests for the Relate transforms.
use super::*;
use crate::studio::{BetaMode, MatrixMode, SeriesUnit, Transform};
use tr_core::Timeframe;

fn near(a: f64, b: f64) {
    assert!((a - b).abs() <= 1e-12 * b.abs().max(1.), "{a} != {b}");
}

#[test]
fn relate_projection_level_hand_values() {
    let (c, b) = project(Transform::Level, 2., 1.5, 10., None, 2.).unwrap();
    near(c, 17.);
    for (got, want) in b.iter().zip([13., 15., 19., 21.]) {
        near(*got, want);
    }
    let (c, b) = project(Transform::Change { h: 5 }, 0.5, 2., 1.25, Some(50.), 0.75).unwrap();
    near(c, 53.);
    for (got, want) in b.iter().zip([51.5, 52.25, 53.75, 54.5]) {
        near(*got, want);
    }
    // Missing anchor or a non-finite result is a gap, never a number.
    assert!(project(Transform::Change { h: 1 }, 0., 1., 1., None, 1.).is_none());
    assert!(project(Transform::LogLevel, 0., 1., 1e6, None, 1.).is_none());
}

#[test]
fn relate_projection_logreturn_hand_values() {
    let sigma = 1.02_f64.ln();
    let (c, b) = project(Transform::LogReturn { h: 5 }, 0., 1., 1.1_f64.ln(), Some(100.), sigma).unwrap();
    near(c, 110.);
    for (got, want) in b.iter().zip([110. / 1.0404, 110. / 1.02, 112.2, 110. * 1.0404]) {
        near(*got, want);
    }
    assert!((b[1] - 107.843137254902).abs() < 1e-9 && (b[0] - 105.728566).abs() < 1e-6);
    // σ is added before exp: the +1σ band is not centre·(1+σ).
    assert!((b[2] - 110. * (1. + sigma)).abs() > 1e-3);
    let (c, b) = project(Transform::LogLevel, 2_f64.ln(), 1., 50_f64.ln(), None, 1.1_f64.ln()).unwrap();
    near(c, 100.);
    for (got, want) in b.iter().zip([100. / 1.21, 100. / 1.1, 110., 121.]) {
        near(*got, want);
    }
    assert!((b[1] - 90.909090909).abs() < 1e-8 && (b[0] - 82.644628099).abs() < 1e-8);
}

#[test]
fn relate_expanding_hand_vector_and_prefix_invariance() {
    let pts: Vec<_> = [(1., 2.), (2., 4.), (3., 6.), (4., 8.5)]
        .iter()
        .enumerate()
        .map(|(i, &(x, y))| (i as i64, y, x, i == 0))
        .collect();
    let e = expanding_beta(&pts);
    assert_eq!(e.iter().map(|p| p.1.is_some()).collect::<Vec<_>>(), [false, false, true, true]);
    near(e[2].1.unwrap(), 2.0);
    near(e[3].1.unwrap(), 2.15);
    // Prefix invariance on a longer seeded series: shared endpoints are bitwise equal.
    let mut s = 7u64;
    let long: Vec<_> = (0..300).map(|i| (i as i64, 0.5 * lcg(&mut s) + 1.3 * 0.1 * i as f64, lcg(&mut s) + 0.05 * i as f64, i == 0)).collect();
    let full = expanding_beta(&long);
    let part = expanding_beta(&long[..180]);
    assert_eq!(&full[..180], &part[..]);
    // A break resets: the first two points after it are unavailable again.
    let mut broken = long.clone();
    broken[100].3 = true;
    let b = expanding_beta(&broken);
    assert!(b[100].1.is_none() && b[101].1.is_none() && b[102].1.is_some());
    assert_eq!(&b[..100], &full[..100]);
    // Latest 1,000 endpoints only.
    let many: Vec<_> = (0..1500).map(|i| (i as i64, (i % 7) as f64, (i % 11) as f64, i == 0)).collect();
    assert_eq!(expanding_beta(&many).len(), MAX_EXPANDING);
}

fn bars(ts: &[i64], closes: &[f64]) -> Vec<Bar> {
    ts.iter()
        .zip(closes)
        .map(|(&t, &c)| Bar::new(t, c, c, c, c, 1.).with_provenance(Provenance::Real))
        .collect()
}
fn named(symbol: &str, bars: Vec<Bar>) -> NamedBars {
    NamedBars { symbol: symbol.into(), bars, source: "unit".into(), ..Default::default() }
}

#[test]
fn corr_common_panel_same_n_all_cells() {
    let ts: Vec<i64> = (1..=6).collect();
    let a = named("A", bars(&ts, &[10., 11., 10.5, 12., 11.5, 13.]));
    let b = named("B", bars(&ts, &[20., 19., 21., 20.5, 22., 21.]));
    let c_ts: Vec<i64> = vec![1, 2, 3, 5, 6];
    let c = named("C", bars(&c_ts, &[5., 5.5, 5.2, 5.9, 5.4]));
    let rows = [a, b, c];
    // Legacy pairwise A–B uses all five adjacent common-price returns.
    let legacy = matrix(&rows, false);
    assert_eq!(legacy.cells[0][1].n, 5);
    let common = common_panel_matrix(&rows, false);
    for i in 0..3 {
        for j in 0..3 {
            assert_eq!(common.cells[i][j].n, 3, "cell {i},{j}");
            assert!(common.cells[i][j].correlation.is_some(), "cell {i},{j}");
        }
    }
    near(common.cells[0][0].correlation.unwrap(), 1.);
    assert_eq!(common.cells[0][1], common.cells[1][0]);
    // 3 → 5 is consecutive in C but not in A/B, so the common returns are 1→2, 2→3, 5→6.
    let r = |bars: &[Bar], i: usize, j: usize| bars[j].close.ln() - bars[i].close.ln();
    let (x, y) = ([r(&rows[0].bars, 0, 1), r(&rows[0].bars, 1, 2), r(&rows[0].bars, 4, 5)], [r(&rows[1].bars, 0, 1), r(&rows[1].bars, 1, 2), r(&rows[1].bars, 4, 5)]);
    let pairs: Vec<_> = x.iter().zip(&y).map(|(x, y)| PairReturn { start: 0, end: 1, asset: *x, benchmark: *y }).collect();
    near(common.cells[0][1].correlation.unwrap(), regress(&pairs, 252.).unwrap().correlation.unwrap());
    // An unavailable row stays visible with its reason and never enters the complete cases.
    let mut with_error = rows.to_vec();
    with_error.push(NamedBars { symbol: "D".into(), error: Some("no archive".into()), ..Default::default() });
    let m = common_panel_matrix(&with_error, false);
    assert_eq!(m.cells[0][1].n, 3);
    assert_eq!(m.cells[3][0].reason.as_deref(), Some("no archive"));
    assert_eq!(m.unavailable.len(), 1);
    let _ = MatrixMode::Common;
}

fn lcg(state: &mut u64) -> f64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    ((*state >> 11) as f64) / (1u64 << 53) as f64 - 0.5
}
fn sessions(n: usize) -> Vec<i64> {
    let mut d: chrono::NaiveDate = "2022-01-03".parse().unwrap();
    (0..n)
        .map(|_| {
            let ts = d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
            d = crate::history::calendar::next_session(d);
            ts
        })
        .collect()
}
/// Dev-only related walks on consecutive cash sessions: asset log return = 0.0002 + β·benchmark + noise.
fn related(n: usize, beta: f64, seed: u64) -> (NamedBars, NamedBars) {
    let ts = sessions(n);
    let mut s = seed;
    let (mut la, mut lb) = (4.6_f64, 5.9_f64);
    let (mut a, mut b) = (vec![], vec![]);
    for _ in 0..n {
        let x = 0.01 * lcg(&mut s);
        lb += x;
        la += 0.0002 + beta * x + 0.004 * lcg(&mut s);
        a.push(la.exp());
        b.push(lb.exp());
    }
    (named("AAPL US Equity", bars(&ts, &a)), named("QQQ US Equity", bars(&ts, &b)))
}
const OPT: Options = Options { annual_periods: 252., daily: true };

fn prep(a: &NamedBars, b: &NamedBars, t: Transform) -> Prepared {
    prepare(a, b, OPT, Timeframe::D1, t, "unit", 0, None).unwrap()
}

#[test]
fn relate_transforms_known_values_and_strict_adjacency() {
    let (a, b) = related(60, 1.2, 3);
    for (t, want_valid) in [
        (Transform::Level, 60),
        (Transform::LogLevel, 60),
        (Transform::Change { h: 1 }, 59),
        (Transform::LogReturn { h: 5 }, 55),
        (Transform::LogReturn { h: 20 }, 40),
    ] {
        let p = prep(&a, &b, t);
        assert_eq!(p.valid_rows().len(), want_valid, "{t:?}");
    }
    let p = prep(&a, &b, Transform::LogReturn { h: 5 });
    near(p.y.values[7].unwrap(), a.bars[7].close.ln() - a.bars[2].close.ln());
    near(p.x.values[7].unwrap(), b.bars[7].close.ln() - b.bars[2].close.ln());
    assert_eq!(p.anchor[7], Some(a.bars[2].close));
    let p = prep(&a, &b, Transform::Change { h: 1 });
    near(p.y.values[9].unwrap(), a.bars[9].close - a.bars[8].close);
    let p = prep(&a, &b, Transform::LogLevel);
    near(p.y.values[3].unwrap(), a.bars[3].close.ln());
    assert!(matches!(&p.y.unit, SeriesUnit::Native(u) if u == "ln price"));
    assert!(prepare(&a, &b, OPT, Timeframe::D1, Transform::Change { h: 7 }, "unit", 0, None).is_err());
    // Sample / unverified inputs are refused before any alignment.
    let mut bad = a.clone();
    bad.bars[10].provenance = Provenance::Sample;
    assert!(prepare(&bad, &b, OPT, Timeframe::D1, Transform::Level, "unit", 0, None).is_err());
}

#[test]
fn fair_value_gap_breaks() {
    let (a, mut b) = related(120, 1.0, 5);
    b.bars.remove(60);
    for (t, h) in [(Transform::Change { h: 1 }, 1usize), (Transform::LogReturn { h: 5 }, 5)] {
        let p = prep(&a, &b, t);
        // Aligned rows drop timestamp 60, so rows 60..60+h now straddle a missing benchmark bar.
        for row in 60..60 + h {
            assert!(p.y.values[row].is_none(), "{t:?} row {row}");
        }
        assert!(p.y.values[60 + h].is_some());
        assert!(p.y.break_before[60 + h], "{t:?}");
        let fit = training_fit(&p, 1, CovarianceMethod::HacFixed5).unwrap();
        let points = fair_value(&p, &fit);
        let after = points.iter().find(|q| q.ts == p.ts[60 + h]).unwrap();
        assert!(after.break_before);
        assert!(points.iter().all(|q| q.ts != p.ts[60]), "no point inside the missing span");
    }
    // A vault gap on either series is spanned by no transform.
    let (mut a, b) = related(120, 1.0, 5);
    a.gaps = vec![(a.bars[70].ts, a.bars[71].ts)];
    let p = prep(&a, &b, Transform::LogReturn { h: 5 });
    for row in 71..76 {
        assert!(p.y.values[row].is_none());
    }
    assert!(p.y.values[76].is_some() && p.y.break_before[76]);
    // Level keeps every point but breaks the line before the row after the gap.
    let p = prep(&a, &b, Transform::Level);
    assert!(p.y.values[71].is_some() && p.y.break_before[71] && !p.y.break_before[72]);
}

#[test]
fn relate_bands_are_not_mean_ci() {
    let (a, b) = related(2000, 1.1, 11);
    for t in [Transform::LogReturn { h: 1 }, Transform::Level, Transform::Change { h: 5 }] {
        let p = prep(&a, &b, t);
        let fit = training_fit(&p, 9, CovarianceMethod::HacFixed5).unwrap();
        assert_eq!(fit.n, p.valid_rows().len() * 4 / 5);
        let points = fair_value(&p, &fit);
        assert_eq!(points.len(), MAX_POINTS.min(p.valid_rows().len()));
        assert!(points.iter().any(|q| !q.in_sample) && points.iter().any(|q| q.in_sample));
        let q = points.last().unwrap();
        let (centre, bands) = project(t, fit.alpha, fit.beta, q.x, q.anchor, fit.sigma).unwrap();
        if matches!(t, Transform::Level | Transform::Change { .. }) {
            // Linear targets: the band half-width is exactly σ in price units.
            near(bands[2] - centre, fit.sigma);
            near(centre - bands[1], fit.sigma);
            near(bands[3] - centre, 2. * fit.sigma);
        }
        assert_eq!((centre, bands), (q.centre, q.bands));
        let h = hover_stats(&fit, q).unwrap();
        if matches!(t, Transform::Level | Transform::Change { .. }) {
            near(h.mean_ci[1] - q.centre, 1.96 * h.se_mean);
        } else {
            // Target-space ±1.96·se_m, inverted: P_{t−h}·exp(a + b r + 1.96 se_m).
            near(h.mean_ci[1], q.anchor.unwrap() * (fit.alpha + fit.beta * q.x + 1.96 * h.se_mean).exp());
        }
        assert!(h.se_prediction > fit.sigma && h.se_prediction > h.se_mean);
        if t == (Transform::LogReturn { h: 1 }) {
            assert!(1.96 * h.se_mean < fit.sigma / 5., "{} vs {}", 1.96 * h.se_mean, fit.sigma);
            // Not the mean CI: the residual band is much wider.
            assert!(bands[2] - centre > 5. * (h.mean_ci[1] - centre));
        }
    }
}

#[test]
fn fit_is_frozen_on_the_training_prefix() {
    let (a, b) = related(400, 1.0, 21);
    let p = prep(&a, &b, Transform::LogReturn { h: 1 });
    let fit = training_fit(&p, 1, CovarianceMethod::HacFixed5).unwrap();
    // Changing only test-span prices never moves the frozen coefficients or σ.
    let mut a2 = a.clone();
    for bar in &mut a2.bars[360..] {
        bar.close *= 1.3;
        bar.open = bar.close;
        bar.high = bar.close;
        bar.low = bar.close;
    }
    let p2 = prep(&a2, &b, Transform::LogReturn { h: 1 });
    let fit2 = training_fit(&p2, 1, CovarianceMethod::HacFixed5).unwrap();
    assert_eq!((fit.alpha, fit.beta, fit.sigma, fit.train_end), (fit2.alpha, fit2.beta, fit2.sigma, fit2.train_end));
    assert_eq!(fit.fit_id, fit2.fit_id);
    // The id moves with the key, the training end and the transform.
    let other = training_fit(&p, 2, CovarianceMethod::HacFixed5).unwrap();
    assert_ne!(fit.fit_id, other.fit_id);
    let p3 = prep(&a, &b, Transform::Change { h: 1 });
    assert_ne!(fit.fit_id, training_fit(&p3, 1, CovarianceMethod::HacFixed5).unwrap().fit_id);
    // Out-of-sample overlay points do not depend on later bars (prefix invariance of the score).
    let pts = fair_value(&p, &fit);
    let short_p = prep(&NamedBars { bars: a.bars[..380].to_vec(), ..a.clone() }, &NamedBars { bars: b.bars[..380].to_vec(), ..b.clone() }, Transform::LogReturn { h: 1 });
    let mut short_fit = fit.clone();
    short_fit.train_end = fit.train_end;
    let short_pts = fair_value(&short_p, &short_fit);
    for q in &short_pts {
        assert_eq!(pts.iter().find(|p| p.ts == q.ts).unwrap().bands, q.bands);
    }
}

#[test]
fn relate_refusals_stay_unavailable() {
    let (a, b) = related(30, 1.0, 1);
    let p = prep(&a, &b, Transform::LogReturn { h: 5 });
    assert!(training_fit(&p, 1, CovarianceMethod::HacFixed5).unwrap_err().contains("training pairs"));
    // A flat benchmark leaves no β.
    let (a, mut b) = related(200, 1.0, 1);
    for bar in &mut b.bars {
        (bar.open, bar.high, bar.low, bar.close) = (100., 100., 100., 100.);
    }
    let p = prep(&a, &b, Transform::LogReturn { h: 1 });
    assert!(training_fit(&p, 1, CovarianceMethod::HacFixed5).is_err());
    let spec = RelateSpec::new("QQQ US Equity", Transform::LogReturn { h: 1 });
    let r = compute_relate(&a, &b, &[], OPT, Timeframe::D1, &spec, "unit", 0, None, 1).unwrap();
    assert!(r.fit.is_none() && r.points.is_empty() && r.unavailable.is_some());
}

#[test]
fn compute_relate_end_to_end_bounds_and_modes() {
    let (a, b) = related(1500, 0.9, 31);
    let rows = [a.clone(), b.clone()];
    let mut spec = RelateSpec::new("QQQ US Equity", Transform::LogReturn { h: 5 });
    spec.beta_mode = BetaMode::Expanding;
    spec.matrix = MatrixMode::Common;
    let r = compute_relate(&a, &b, &rows, OPT, Timeframe::D1, &spec, "unit", 7, Some(9), 42).unwrap();
    assert!(r.fit.is_some() && r.points.len() <= MAX_POINTS);
    assert_eq!(r.expanding.len(), MAX_EXPANDING);
    assert!(r.matrix_common.as_ref().unwrap().cells[0][1].n > 1000);
    assert_eq!(r.y.ts.len(), 1500);
    assert_eq!((r.y.receipt.closed_thru, r.y.receipt.observed_at), (7, Some(9)));
    let mut off = spec.clone();
    off.beta_mode = BetaMode::Rolling;
    off.matrix = MatrixMode::Pairwise;
    let r = compute_relate(&a, &b, &rows, OPT, Timeframe::D1, &off, "unit", 7, Some(9), 42).unwrap();
    assert!(r.expanding.is_empty() && r.matrix_common.is_none());
}

#[test]
fn corr_pairwise_legacy_unchanged() {
    // Legacy CORR/RBETA never builds a Relate report and serialises exactly as before: no `relate`
    // key, pairwise matrix, common-timestamp returns, with gaps present.
    let (mut a, mut b) = related(400, 0.9, 17);
    b.bars.remove(150);
    a.gaps = vec![(a.bars[200].ts, a.bars[201].ts)];
    let rows = [a.clone(), b.clone(), named("PEER US Equity", related(400, 0.5, 19).0.bars)];
    for h in [1usize, 5, 20] {
        let r = compute_series_at_horizon(&a, &b, &rows, OPT, "unit", h).unwrap();
        assert!(r.relate.is_none());
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("\"relate\""), "legacy export gained a relate key");
        let again = serde_json::to_string(&compute_series_at_horizon(&a, &b, &rows, OPT, "unit", h).unwrap()).unwrap();
        assert_eq!(json, again, "legacy report must be deterministic at h={h}");
        // Computing a Relate report on the same inputs leaves the legacy numbers untouched.
        let mut with = r.clone();
        let spec = RelateSpec::new("QQQ US Equity", Transform::LogReturn { h });
        with.relate = Some(Box::new(compute_relate(&a, &b, &rows, OPT, Timeframe::D1, &spec, "unit", 0, None, 1).unwrap()));
        assert!(serde_json::to_string(&with).unwrap().contains("\"relate\""));
        with.relate = None;
        assert_eq!(serde_json::to_string(&with).unwrap(), json);
        // F3: the residual path is only cumulated at h=1.
        assert_eq!(r.path.iter().all(|p| p.residual_return.is_none()), h > 1, "h={h}");
    }
}
