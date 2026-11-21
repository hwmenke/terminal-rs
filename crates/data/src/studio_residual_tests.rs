//! Known answers, no-refit, prefix-invariance, gap, monitor, revision and refusal tests for RESID.
use super::*;
use crate::studio_corr::{Options, RelateSpec, compute_relate, compute_series_at_horizon};
use crate::studio_linalg::FIT_CALLS;

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
fn level(symbol: &str) -> FactorSpec {
    FactorSpec { symbol: symbol.into(), transform: Transform::Level, lag: 0 }
}

/// A hand-built Level model (no fit anywhere): `ŷ = β·[1, x…]`, trained through day `trained_idx`.
fn manual_fit(
    live: &[&NamedBars],
    beta: &[f64],
    mu: f64,
    sigma: f64,
    trained_idx: usize,
    monitor: usize,
) -> FrozenFit {
    let k = beta.len();
    let thru = T0 + trained_idx as i64 * DAY;
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
            target: Transform::Level,
            factors: live[1..].iter().map(|r| level(&r.symbol)).collect(),
            coefficients: beta.to_vec(),
            n_train: 120,
            k,
            train_start: T0,
            ..Default::default()
        },
        residual_mean: mu,
        residual_sigma: sigma,
        trained_thru: thru,
        embargo_bars: 0,
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
        monitoring_horizon: monitor,
        bootstrap_block: 5,
        ..Default::default()
    };
    fit.content_digest = fit.compute_digest();
    fit.validate().unwrap();
    fit
}
fn run(fit: &FrozenFit, t: &NamedBars, f: &[NamedBars]) -> ResidualReport {
    score(fit, t, f, true, "unit", 0, None, 1)
}
fn noise_level(n: usize, seed: u64, vol: f64) -> (Vec<f64>, Vec<f64>) {
    let mut s = seed;
    let x: Vec<f64> = (0..n).map(|_| 5. + 3. * lcg(&mut s)).collect();
    let y = x.iter().map(|x| 1. + 2. * x + vol * (lcg(&mut s) - 0.5)).collect();
    (y, x)
}

#[test]
fn residual_saved_coefficients_never_refit() {
    // V1: β = [1, 2], μ = 0, σ = 2, x = 4, y = 12 → ŷ = 9, e = 3, z = 1.5.
    let (t, f) = (named(TGT, &[12.; 30]), named(F1, &[4.; 30]));
    let fit = manual_fit(&[&t, &f], &[1., 2.], 0., 2., 9, 20);
    let before = FIT_CALLS.with(|c| c.get());
    let r = run(&fit, &t, &[f.clone()]);
    assert!(r.unavailable.is_none());
    for i in 0..30 {
        close(r.residual.values[i].unwrap(), 3., 1e-12);
        close(r.z.values[i].unwrap(), 1.5, 1e-12);
        close(r.fitted[i].unwrap(), 9., 1e-12);
    }
    // V3: β = [0.1, 2, −1], x = (0.3, 0.2), y = 0.9 → ŷ = 0.5, e = 0.4.
    let (t3, a, b) = (named(TGT, &[0.9; 30]), named(F1, &[0.3; 30]), named(F2, &[0.2; 30]));
    let fit3 = manual_fit(&[&t3, &a, &b], &[0.1, 2., -1.], 0., 1., 9, 20);
    close(run(&fit3, &t3, &[a, b]).residual.values[5].unwrap(), 0.4, 1e-12);
    // A live series whose fresh OLS slope differs: the residual is y − saved fit, not the fresh one.
    let (y, x) = noise_level(200, 3, 0.3);
    let steep: Vec<f64> = y.iter().zip(&x).map(|(y, x)| y + 4. * x).collect();
    let (t, f) = (named(TGT, &steep), named(F1, &x));
    let r = run(&fit, &t, &[f]);
    for i in 0..200 {
        close(r.residual.values[i].unwrap(), steep[i] - (1. + 2. * x[i]), 1e-12);
    }
    let _ = checks(&fit, &r);
    assert_eq!(FIT_CALLS.with(|c| c.get()), before, "score and checks must never call fit_ols");
}

#[test]
fn residual_saved_scale_never_changes() {
    let (y, x) = noise_level(100, 5, 0.2);
    let (t, f) = (named(TGT, &y), named(F1, &x));
    let fit = manual_fit(&[&t, &f], &[1., 2.], 0.5, 2., 49, 20);
    let (mu, sigma, digest) = (fit.residual_mean, fit.residual_sigma, fit.content_digest);
    // Append 200 rows with 10× the volatility.
    let (y2, x2) = noise_level(300, 5, 2.0);
    let big = run(&fit, &named(TGT, &y2), &[named(F1, &x2)]);
    assert_eq!((fit.residual_mean, fit.residual_sigma, fit.content_digest), (mu, sigma, digest));
    for i in 0..300 {
        let e = y2[i] - (1. + 2. * x2[i]);
        close(big.residual.values[i].unwrap(), e, 1e-12);
        close(big.z.values[i].unwrap(), (e - 0.5) / 2., 1e-12);
    }
    // V2: μ = 0.5 → z = 1.25 for e = 3.
    let (t, f) = (named(TGT, &[12.; 30]), named(F1, &[4.; 30]));
    let fit = manual_fit(&[&t, &f], &[1., 2.], 0.5, 2., 9, 20);
    close(run(&fit, &t, &[f]).z.values[3].unwrap(), 1.25, 1e-12);
}

#[test]
fn residual_append_preserves_prefix() {
    let (y, x) = noise_level(180, 9, 0.4);
    let mut y_shock = y.clone();
    y_shock[150..].iter_mut().for_each(|v| *v += 40.); // a shock in the appended rows
    let fit = manual_fit(&[&named(TGT, &y), &named(F1, &x)], &[1., 2.], 0., 1., 59, 20);
    let a = run(&fit, &named(TGT, &y[..120]), &[named(F1, &x[..120])]);
    let b = run(&fit, &named(TGT, &y_shock), &[named(F1, &x)]);
    for i in 0..120 {
        assert_eq!(a.residual.values[i].map(f64::to_bits), b.residual.values[i].map(f64::to_bits));
        assert_eq!(a.z.values[i].map(f64::to_bits), b.z.values[i].map(f64::to_bits));
        assert_eq!((a.segment[i], a.residual.break_before[i]), (b.segment[i], b.residual.break_before[i]));
        assert_eq!(a.residual.ts[i], b.residual.ts[i]);
    }
    assert_eq!((a.scored.len(), b.scored.len()), (60, 120));
    assert!(b.scored.iter().any(|e| *e > 30.));
    assert_eq!(a.segment[59], ResidSegment::Train);
    assert_eq!(a.segment[60], ResidSegment::Scored);
}

fn level_pair(n: usize, seed: u64) -> (NamedBars, NamedBars) {
    let (y, x) = noise_level(n, seed, 1.0);
    (named(TGT, &y), named(F1, &x))
}

#[test]
fn residual_check_waits_for_scored_n() {
    // 100 training bars are never counted; 23 scored rows stay pending.
    let (t, f) = level_pair(123, 21);
    let fit = manual_fit(&[&t, &f], &[1., 2.], 0., 1., 99, 20);
    let c = checks(&fit, &run(&fit, &t, &[f]));
    assert!(matches!(c.adf, CheckState::Pending { have: 23, need: 60, .. }), "{:?}", c.adf);
    assert_eq!(adf_pending_text(23, 60), "Stationarity check pending: 23/60 scored bars");
    // A partial monitor exists but never a verdict.
    match &c.monitor {
        CheckState::Ready(m) => assert!(m.k == 20 && m.p.is_some() || m.k < m.m),
        other => panic!("{other:?}"),
    }
    let (t, f) = level_pair(160, 21);
    let fit = manual_fit(&[&t, &f], &[1., 2.], 0., 1., 99, 20);
    let c = checks(&fit, &run(&fit, &t, &[f]));
    match c.adf {
        CheckState::Ready(a) => assert_eq!(a.t_len, 60),
        other => panic!("{other:?}"),
    }
    assert!(matches!(c.half_life, CheckState::Ready(_)));
}

#[test]
fn residual_embargo_rows_never_count() {
    let (t, f) = level_pair(160, 22);
    let mut fit = manual_fit(&[&t, &f], &[1., 2.], 0., 1., 99, 20);
    // An embargo is only valid with an h-bar target: h = 5 → 4 embargo bars.
    fit.model.target = Transform::Change { h: 5 };
    fit.embargo_bars = 4;
    fit.content_digest = fit.compute_digest();
    fit.validate().unwrap();
    let r = run(&fit, &t, &[f]);
    assert!(r.unavailable.is_none(), "{:?}", r.unavailable);
    let seg_at = |ts: i64| r.segment[r.residual.ts.iter().position(|t| *t == ts).unwrap()];
    assert_eq!(seg_at(T0 + 99 * DAY), ResidSegment::Train);
    assert!((100..104).all(|i| seg_at(T0 + i * DAY) == ResidSegment::Embargo));
    assert_eq!(seg_at(T0 + 104 * DAY), ResidSegment::Scored);
    assert_eq!(r.scored.len(), 160 - 104);
    assert_eq!(r.scored_ts[0], T0 + 104 * DAY);
}

#[test]
fn residual_gap_segments_adf() {
    let (t, f) = level_pair(240, 31);
    let fit = manual_fit(&[&t, &f], &[1., 2.], 0., 1., 59, 20);
    // A gap at day 200: the latest run is days 201..240 = 39 rows.
    let r = run(&fit, &t, &[drop_day(f.clone(), 200)]);
    assert!(r.scored_breaks.iter().filter(|b| **b).count() >= 2);
    assert_eq!(r.dropped.iter().map(|(_, n)| *n).sum::<usize>() > 0, true);
    let c = checks(&fit, &r);
    assert!(matches!(c.adf, CheckState::Pending { have: 39, need: 60, .. }), "{:?}", c.adf);
    // A gap at day 100 leaves a 139-row latest run; t_len equals the run length.
    let r = run(&fit, &t, &[drop_day(f, 100)]);
    match checks(&fit, &r).adf {
        CheckState::Ready(a) => {
            assert_eq!(a.t_len, 139);
            assert_eq!(a.window.0, T0 + 101 * DAY);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn residual_monitor_horizon_is_fixed() {
    let (t, f) = level_pair(200, 41);
    // m = 40; 25 scored rows → partial; 40 → a verdict; 100 more → bitwise equal.
    let fit = manual_fit(&[&t, &f], &[1., 2.], 0., 1., 99, 40);
    let at = |n: usize| {
        let (tn, fnn) = (NamedBars { bars: t.bars[..n].to_vec(), ..t.clone() }, NamedBars { bars: f.bars[..n].to_vec(), ..f.clone() });
        match checks(&fit, &run(&fit, &tn, &[fnn])).monitor {
            CheckState::Ready(m) => m,
            other => panic!("{other:?}"),
        }
    };
    let partial = at(125);
    assert_eq!((partial.k, partial.m, partial.p), (25, 40, None));
    assert_eq!(partial.path.len(), 25);
    assert_eq!(partial.envelope.len(), 40);
    let full = at(140);
    assert_eq!(full.k, 40);
    assert!(full.p.is_some() && full.p.unwrap() > 0. && full.p.unwrap() <= 1.);
    let longer = at(200);
    assert_eq!(longer.k, 40);
    assert_eq!(
        (full.d_k.to_bits(), full.critical.to_bits(), full.p.map(f64::to_bits)),
        (longer.d_k.to_bits(), longer.critical.to_bits(), longer.p.map(f64::to_bits))
    );
    assert_eq!(full, at(140), "same digest ⇒ identical draws");
    // The critical value depends on the horizon and the seed only, so the partial one matches.
    assert_eq!(partial.critical.to_bits(), full.critical.to_bits());
    // A different digest is a different seed.
    let mut other = fit.clone();
    other.train_residuals[0] += 1e-3;
    other.content_digest = other.compute_digest();
    let r = run(&other, &t, &[f.clone()]);
    let CheckState::Ready(o) = checks(&other, &r).monitor else { panic!() };
    assert_ne!(o.critical.to_bits(), full.critical.to_bits());
    // V8: e = [1, −1, 2, 0], μ = 0.5 → S = [0.5, −1, 0.5, 0], D₄ = 1.
    let (path, d) = cusum_path(&[1., -1., 2., 0.], 0.5);
    assert_eq!(path, [0.5, -1., 0.5, 0.]);
    assert_eq!(d, 1.);
    // V9: draws 1…499, D = 480 → critical 475, p = 21/500.
    let draws: Vec<f64> = (1..=499).map(f64::from).collect();
    let (crit, p) = fixed_horizon_verdict(480., &draws);
    assert_eq!(crit, 475.);
    assert_eq!(p, 21. / 500.);
}

#[test]
fn residual_revision_preserves_model() {
    let (y, x) = noise_level(160, 51, 1.0);
    let (t, f) = (named(TGT, &y), named(F1, &x));
    let fit = manual_fit(&[&t, &f], &[1., 2.], 0., 1., 99, 20);
    let base = run(&fit, &t, &[f.clone()]);
    assert!(base.revision.iter().all(|(_, c)| *c == InputCheck::Unchanged));
    let key0 = score_key(&fit, &t, &[f.clone()], "unit", true, 0);
    let mut edited = f.clone();
    edited.bars[40].close += 0.5; // inside the training span
    let snapshot = fit.clone();
    let after = run(&fit, &t, &[edited.clone()]);
    assert_eq!(fit, snapshot);
    assert_eq!(fit.compute_digest(), fit.content_digest);
    let rev = after.revision.iter().find(|(s, _)| s == F1).unwrap();
    assert!(matches!(rev.1, InputCheck::Revised { .. }), "{rev:?}");
    assert_eq!(after.revision[0].1, InputCheck::Unchanged);
    assert_eq!(base.scored, after.scored, "scored residuals do not depend on a past factor close at lag 0");
    assert_ne!(score_key(&fit, &t, &[edited], "unit", true, 0), key0);
    // A live window that starts after the training span is "not covered".
    let short = NamedBars { bars: f.bars[30..].to_vec(), ..f.clone() };
    let r = run(&fit, &t, &[short]);
    assert!(matches!(r.revision[1].1, InputCheck::NotCovered { .. }));
    // A different source label is reported, not hidden.
    let moved = NamedBars { source: "other".into(), ..f };
    assert!(matches!(run(&fit, &t, &[moved]).revision[1].1, InputCheck::SourceChanged { .. }));
}

#[test]
fn residual_missing_factor_never_reestimates() {
    let (t, f) = level_pair(120, 61);
    let fit = manual_fit(&[&t, &f], &[1., 2.], 0., 1., 59, 20);
    let broken = NamedBars { error: Some("no archive".into()), ..f.clone() };
    let r = run(&fit, &t, &[broken]);
    assert!(r.unavailable.as_ref().unwrap().contains("FFF"), "{:?}", r.unavailable);
    assert_eq!(r.factor_errors.len(), 1);
    assert!(r.residual.ts.is_empty());
    // A missing timestamp gives None at that bar.
    let r = run(&fit, &t, &[drop_day(f, 80)]);
    let at = r.residual.ts.iter().position(|ts| *ts == T0 + 80 * DAY).unwrap();
    assert!(r.residual.values[at].is_none() && r.z.values[at].is_none());
    assert!(r.residual.values[at + 1].is_some() && r.residual.break_before[at + 1]);
}

#[test]
fn residual_score_key_excludes_wall_clock() {
    let (t, f) = level_pair(100, 71);
    let fit = manual_fit(&[&t, &f], &[1., 2.], 0., 1., 59, 20);
    let key = |t: &NamedBars, f: &NamedBars, src: &str| score_key(&fit, t, &[f.clone()], src, true, 5).params;
    let base = key(&t, &f, "unit");
    // observed_at never enters the key (it is a report field only).
    let a = score(&fit, &t, &[f.clone()], true, "unit", 5, Some(1), base);
    let b = score(&fit, &t, &[f.clone()], true, "unit", 5, Some(999_999), base);
    assert_eq!(a.residual.values, b.residual.values);
    assert_ne!(a.observed_at, b.observed_at);
    let mut old = f.clone();
    old.bars[3].close += 0.01;
    assert_ne!(base, key(&t, &old, "unit"));
    assert_ne!(base, key(&t, &drop_day(f.clone(), 50), "unit"));
    assert_ne!(base, key(&t, &f, "other"));
    let mut sample = f.clone();
    sample.bars[0] = sample.bars[0].with_provenance(Provenance::Sample);
    assert_ne!(base, key(&t, &sample, "unit"));
    assert_ne!(base, score_key(&fit, &t, &[f.clone()], "unit", true, 6).params);
    let mut other = fit.clone();
    other.content_digest ^= 1;
    assert_ne!(base, score_key(&other, &t, &[f], "unit", true, 5).params);
    // Same inputs, same key, every time.
    let (t2, f2) = level_pair(100, 71);
    assert_eq!(score_key(&fit, &t, &[f2.clone()], "unit", true, 5), score_key(&fit, &t2, &[f2], "unit", true, 5));
}

#[test]
fn residual_series_is_signed_scalar() {
    let (t, f) = level_pair(120, 81);
    let fit = manual_fit(&[&t, &f], &[1., 2.], 0., 1., 59, 20);
    let r = run(&fit, &t, &[f]);
    assert!(r.residual.values.iter().flatten().any(|e| *e < 0.), "a residual must be able to be negative");
    assert_eq!(r.residual.unit, unit_for(Transform::Level));
    assert_eq!(r.z.unit, SeriesUnit::ZFrozen);
    assert_eq!(r.z.unit.label(), "z (saved training σ)");
    assert_eq!(r.point(70).segment, ResidSegment::Scored);
    assert_eq!(r.point(70).residual, r.residual.values[70]);
}

// ───────────────────────── freeze: Lab and Relate ─────────────────────────

fn related(n: usize, beta: f64, seed: u64) -> (NamedBars, NamedBars) {
    let mut s = seed;
    let mut p = 100.;
    let f: Vec<f64> = (0..n)
        .map(|_| {
            p *= (0.02 * (lcg(&mut s) - 0.5)).exp();
            p
        })
        .collect();
    let mut q = 50.;
    let tgt: Vec<f64> = (0..n)
        .map(|i| {
            if i > 0 {
                q *= (beta * (f[i] / f[i - 1]).ln() + 0.004 * (lcg(&mut s) - 0.5)).exp();
            }
            q
        })
        .collect();
    (named(TGT, &tgt), named(F1, &f))
}

fn lab_report(n: usize) -> (LabReport, NamedBars, NamedBars) {
    let (t, f) = related(n, 0.8, 5);
    let spec = LabSpec { factors: vec![FactorSpec::new(F1, Transform::LogReturn { h: 1 }, 0)], ..Default::default() };
    let r = studio_lab::compute(&t, &[f.clone()], &spec, Timeframe::D1, true, "unit", 0, Some(5), 7);
    assert!(r.unavailable.is_none(), "{:?}", r.unavailable);
    (r, t, f)
}

#[test]
fn residual_freeze_lab_roundtrip_scores_like_the_lab() {
    let (r, t, f) = lab_report(500);
    let fit = freeze_lab(&r, None, None, 123).unwrap();
    assert_eq!(fit.name, "TTT-v-FFF");
    assert_eq!(fit.model.k, 2);
    assert_eq!(fit.monitoring_horizon, DEFAULT_MONITOR.min(fit.train_residuals.len()));
    assert_eq!(fit.embargo_bars, 0);
    fit.validate().unwrap();
    // Through JSON: identical content, identical digest.
    let back: FrozenFit = serde_json::from_str(&serde_json::to_string(&fit).unwrap()).unwrap();
    assert_eq!(back, fit);
    back.validate().unwrap();
    // Scoring the same inputs reproduces the Lab's own frozen-coefficient residuals.
    let s = score(&fit, &t, &[f], true, "unit", 0, Some(5), 1);
    for p in &r.points {
        let i = s.residual.ts.iter().position(|ts| *ts == p.ts).unwrap();
        close(s.residual.values[i].unwrap(), p.residual, 1e-12);
    }
    // The training residuals are the tail of the in-sample residuals; the mean is over all rows.
    close(fit.residual_mean, r.train_residual_mean, 0.);
    assert!(fit.residual_mean.abs() < 1e-12);
    // A tampered coefficient fails the digest; a changed name/id/parent/saved_at does not.
    let mut bad = fit.clone();
    bad.model.coefficients[1] += 1e-9;
    assert!(bad.validate().unwrap_err().contains("digest"));
    let mut renamed = fit.clone();
    (renamed.id, renamed.name, renamed.parent, renamed.saved_at) = ("x-v2".into(), "x".into(), Some("x-v1".into()), 9);
    renamed.validate().unwrap();
}

#[test]
fn residual_relate_save_matches_fair_value() {
    let (mut a, mut b) = related(500, 0.9, 13);
    // Per-symbol provider labels, as live rescoring sees them (the joined CORR string is not one of them).
    a.source = "ProvA".into();
    b.source = "ProvB · VAULT 400 + LIVE 3 bars · 0 holes".into();
    let opts = Options { annual_periods: 252., daily: true };
    let rows = [a.clone(), b.clone()];
    let mut corr = compute_series_at_horizon(&a, &b, &rows, opts, "ProvA + ProvB", 1).unwrap();
    let spec = RelateSpec::new(F1, Transform::LogReturn { h: 1 });
    corr.relate = Some(Box::new(compute_relate(&a, &b, &rows, opts, Timeframe::D1, &spec, "ProvA + ProvB", 0, None, 1).unwrap()));
    let rel = corr.relate.as_ref().unwrap();
    let fit = freeze_relate(&corr, Timeframe::D1, None, Some(40), 1).unwrap();
    assert_eq!(fit.training_receipts[0].source, "ProvA");
    assert_eq!(fit.training_receipts[1].source, b.source);
    // Rescoring with the same per-symbol sources (a changed intraday stitch note included): all Unchanged.
    let same = score(&fit, &a, &[b.clone()], true, "ProvA", 0, None, 1);
    assert!(same.revision.iter().all(|(_, c)| *c == InputCheck::Unchanged), "{:?}", same.revision);
    let restitched = NamedBars { source: "ProvB · VAULT 401 + LIVE 4 bars · 1 holes".into(), ..b.clone() };
    let again = score(&fit, &a, &[restitched], true, "ProvA", 0, None, 1);
    assert!(again.revision.iter().all(|(_, c)| *c == InputCheck::Unchanged), "{:?}", again.revision);
    let moved = NamedBars { source: "ProvZ".into(), ..b.clone() };
    let changed = score(&fit, &a, &[moved], true, "ProvA", 0, None, 1);
    assert!(matches!(changed.revision[1].1, InputCheck::SourceChanged { .. }), "{:?}", changed.revision);
    assert_eq!(fit.model.origin, FitOrigin::Relate);
    assert_eq!(fit.model.factors, [FactorSpec { symbol: F1.into(), transform: Transform::LogReturn { h: 1 }, lag: 0 }]);
    close(fit.residual_sigma, rel.fit.as_ref().unwrap().sigma, 1e-15);
    let s = score(&fit, &a, &[b], true, "unit", 0, None, 1);
    let rows_scored: Vec<usize> = (0..s.residual.ts.len()).filter(|i| s.z.values[*i].is_some()).collect();
    assert_eq!(rows_scored.len(), rel.points.len(), "the rows are the same set as the FairValuePoints");
    for p in &rel.points {
        let i = s.residual.ts.iter().position(|t| *t == p.ts).expect("row present");
        close(s.z.values[i].unwrap(), p.z, 1e-9);
    }
    // Embargo / training flags agree too.
    assert!(rel.points.iter().all(|p| p.in_sample == (s.segment[s.residual.ts.iter().position(|t| *t == p.ts).unwrap()] == ResidSegment::Train)));
}

#[test]
fn residual_freeze_refusals() {
    let (r, _, _) = lab_report(500);
    assert!(freeze_lab(&r, None, Some(10_000), 0).is_err(), "monitor out of range");
    let stored = r.train_residuals.len();
    assert!(freeze_lab(&r, None, Some(MAX_MONITOR.min(stored) + 1), 0).is_err());
    let mut x = r.clone();
    x.provenance = Provenance::Sample;
    assert!(freeze_lab(&x, None, None, 0).unwrap_err().contains("refused"));
    let mut x = r.clone();
    x.unavailable = Some("nope".into());
    assert!(freeze_lab(&x, None, None, 0).unwrap_err().contains("nope"));
    let mut x = r.clone();
    x.fit = None;
    assert!(freeze_lab(&x, None, None, 0).is_err());
    // Empty new fields: a report from an older build.
    let mut x = r.clone();
    (x.train_residuals, x.train_breaks, x.train_span_digests) = (vec![], vec![], vec![]);
    assert!(freeze_lab(&x, None, None, 0).unwrap_err().contains("predates"));
    // A k / receipts mismatch.
    let mut x = r.clone();
    x.receipts.pop();
    assert!(freeze_lab(&x, None, None, 0).unwrap_err().contains("predates"));
    // LOW N.
    let (small, _, _) = lab_report(70);
    assert!(freeze_lab(&small, None, None, 0).unwrap_err().contains("LOW N"));
    // Names.
    assert!(freeze_lab(&r, Some("bad name"), None, 0).is_err());
    assert!(freeze_lab(&r, Some("ok_name-1"), Some(20), 0).is_ok());
    // No Relate section.
    let (a, b) = related(300, 0.9, 13);
    let corr = compute_series_at_horizon(&a, &b, &[a.clone(), b.clone()], Options { annual_periods: 252., daily: true }, "unit", 1).unwrap();
    assert!(freeze_relate(&corr, Timeframe::D1, None, None, 0).is_err());
}

#[test]
fn resid_args_grammar_and_ids() {
    use ResidArgs::*;
    assert_eq!(ResidArgs::parse("").unwrap(), List);
    assert_eq!(ResidArgs::parse("save").unwrap(), Save { name: None, monitor: None });
    assert_eq!(
        ResidArgs::parse("save name=\"AAPL-v-SPY\" monitor=250").unwrap(),
        Save { name: Some("AAPL-v-SPY".into()), monitor: Some(250) }
    );
    assert_eq!(ResidArgs::parse("name=a_b").unwrap(), Save { name: Some("a_b".into()), monitor: None });
    assert_eq!(ResidArgs::parse("open AAPL-v-SPY-v1").unwrap(), Open("AAPL-v-SPY-v1".into()));
    assert_eq!(ResidArgs::parse("OPEN x-v12").unwrap(), Open("x-v12".into()));
    assert_eq!(ResidArgs::parse("check").unwrap(), Check(None));
    assert_eq!(ResidArgs::parse("check x-v2").unwrap(), Check(Some("x-v2".into())));
    for bad in ["open", "open ../x", "open a.b-v1", "save monitor=5", "save monitor=1001", "save name=a b", "save name=", "save x=1", "check a b", "frob", "save name=a name=b", "open x-v0", "open x-v123456"] {
        assert!(ResidArgs::parse(bad).is_err(), "{bad}");
    }
    assert!(valid_id("a-v1") && valid_id("A_b-9-v99999") && !valid_id("a-v") && !valid_id("-v1") && !valid_id("a/b-v1"));
    assert!(!valid_id(&format!("{}-v1", "a".repeat(41))) && valid_id(&format!("{}-v1", "a".repeat(40))));
}

#[test]
fn residual_relate_save_uses_report_timeframe() {
    let (a, b) = related(500, 0.9, 13);
    let opts = Options { annual_periods: 252., daily: true };
    let rows = [a.clone(), b.clone()];
    let mut corr = compute_series_at_horizon(&a, &b, &rows, opts, "unit", 1).unwrap();
    let spec = RelateSpec::new(F1, Transform::LogReturn { h: 1 });
    corr.relate = Some(Box::new(compute_relate(&a, &b, &rows, opts, Timeframe::D1, &spec, "unit", 0, None, 1).unwrap()));
    assert_eq!(corr.relate.as_ref().unwrap().tf, Some(Timeframe::D1));
    // The pane's text box now says 5m while the report is daily: refused, not saved as 5m.
    let err = freeze_relate(&corr, Timeframe::M5, None, None, 0).unwrap_err();
    assert!(err.contains("save refused") && err.contains("Reload"), "{err}");
    assert_eq!(freeze_relate(&corr, Timeframe::D1, None, None, 0).unwrap().model.tf, Timeframe::D1);
    // A report from an older build carries no timeframe.
    corr.relate.as_mut().unwrap().tf = None;
    assert!(freeze_relate(&corr, Timeframe::D1, None, None, 0).unwrap_err().contains("predates"));
}

#[test]
fn residual_source_identity_ignores_intraday_counts() {
    let a = "Yahoo · VAULT 1000 + LIVE 20 bars · 12 sessions · from 2026-01-02 · 0 holes · 0 missing";
    let b = "Yahoo · VAULT 1000 + LIVE 25 bars · 13 sessions · from 2026-01-02 · 1 holes · 0 missing · VAULT ONLY · feed down · note";
    assert_eq!(source_identity(a), "Yahoo");
    assert_eq!(source_identity(a), source_identity(b));
    let (y, x) = noise_level(160, 51, 1.0);
    let (t, f) = (named(TGT, &y), named(F1, &x));
    let mut saved = f.clone();
    saved.source = a.into();
    let fit = manual_fit(&[&t, &saved], &[1., 2.], 0., 1., 99, 20);
    let same = NamedBars { source: b.into(), ..f.clone() };
    assert_eq!(run(&fit, &t, &[same]).revision[1].1, InputCheck::Unchanged);
    let other = NamedBars { source: b.replace("Yahoo", "Other"), ..f };
    assert!(matches!(run(&fit, &t, &[other]).revision[1].1, InputCheck::SourceChanged { .. }));
}

#[test]
fn residual_digest_is_explicit_and_ignores_observed_at() {
    let (t, f) = (named(TGT, &[12.; 30]), named(F1, &[4.; 30]));
    let mut fit = manual_fit(&[&t, &f], &[1., 2.], 0., 2., 9, 20);
    fit.training_receipts[0].input_digest = 5;
    fit.content_digest = fit.compute_digest();
    let base = fit.compute_digest();
    let mut x = fit.clone();
    x.training_receipts[0].observed_at = Some(1_700_000_000);
    x.training_receipts[1].observed_at = Some(7);
    assert_eq!(x.compute_digest(), base, "observed_at is a wall clock and is not content");
    x.validate().unwrap();
    let mut x = fit.clone();
    x.model.coefficients[1] = f64::from_bits(x.model.coefficients[1].to_bits() + 1);
    assert_ne!(x.compute_digest(), base, "one ULP of a coefficient");
    let mut x = fit.clone();
    x.training_receipts[0].input_digest += 1;
    assert_ne!(x.compute_digest(), base);
    let mut x = fit.clone();
    x.training_receipts[1].gaps.push((1, 2));
    assert_ne!(x.compute_digest(), base);
    let mut x = fit.clone();
    x.model.source_report_key ^= 1;
    assert_ne!(x.compute_digest(), base);
    let mut x = fit.clone();
    x.model.tf = Timeframe::W1;
    assert_ne!(x.compute_digest(), base);
    let mut x = fit.clone();
    x.model.factors[0].lag = 1;
    assert_ne!(x.compute_digest(), base);
}

#[test]
fn residual_validate_rejects_inconsistent_bookkeeping() {
    let (t, f) = (named(TGT, &[12.; 30]), named(F1, &[4.; 30]));
    let fit = manual_fit(&[&t, &f], &[1., 2.], 0., 2., 9, 20);
    let reject = |edit: &dyn Fn(&mut FrozenFit), what: &str| {
        let mut x = fit.clone();
        edit(&mut x);
        x.content_digest = x.compute_digest(); // a recomputed digest must not rescue it
        assert!(x.validate().is_err(), "{what} should be rejected");
    };
    reject(&|x| x.embargo_bars = 3, "embargo != h-1");
    reject(&|x| x.embargo_bars = usize::MAX, "overflowing embargo");
    reject(&|x| x.train_breaks[0] = false, "first break false");
    reject(&|x| x.model.n_train = 100, "stored > n_train");
    reject(&|x| x.train_residuals_dropped = 1, "dropped inconsistent");
    reject(&|x| x.bootstrap_block = 0, "block 0");
    reject(&|x| x.bootstrap_block = 121, "block > stored");
    let mut short = fit.clone();
    (short.model.target, short.embargo_bars, short.bootstrap_block) = (Transform::LogReturn { h: 3 }, 2, 2);
    short.content_digest = short.compute_digest();
    assert!(short.validate().unwrap_err().contains("shorter than the return horizon"));
    reject(
        &|x| {
            x.train_residuals = vec![0.1; MAX_TRAIN_RESIDUALS + 1];
            x.train_breaks = vec![false; MAX_TRAIN_RESIDUALS + 1];
            x.train_breaks[0] = true;
            x.model.n_train = MAX_TRAIN_RESIDUALS + 1;
        },
        "stored > MAX",
    );
}

#[test]
fn residual_covariance_saved_and_digest_compatible() {
    // Lab: four entries, equal to the Lab's own covariances.
    let (r, _, _) = lab_report(500);
    let fit = freeze_lab(&r, None, None, 1).unwrap();
    assert_eq!(fit.coef_covariance.len(), 4);
    for (c, i) in fit.coef_covariance.iter().zip(&r.inference) {
        assert_eq!(c, &i.covariance);
    }
    assert!(fit.coef_covariance.iter().any(|c| c.covariance.is_some()));
    // Relate: the fit's single covariance.
    let (a, b) = related(500, 0.9, 13);
    let opts = Options { annual_periods: 252., daily: true };
    let rows = [a.clone(), b.clone()];
    let mut corr = compute_series_at_horizon(&a, &b, &rows, opts, "src", 1).unwrap();
    let spec = RelateSpec::new(F1, Transform::LogReturn { h: 1 });
    corr.relate = Some(Box::new(compute_relate(&a, &b, &rows, opts, Timeframe::D1, &spec, "src", 0, None, 1).unwrap()));
    let rf = freeze_relate(&corr, Timeframe::D1, None, Some(40), 1).unwrap();
    assert_eq!(rf.coef_covariance.len(), 1);
    assert_eq!(rf.coef_covariance[0], corr.relate.as_ref().unwrap().fit.as_ref().unwrap().covariance);
    // A model saved before slice 7 (no `coef_covariance` key) keeps exactly its digest.
    let mut old = fit.clone();
    old.coef_covariance.clear();
    old.content_digest = old.compute_digest();
    let mut v = serde_json::to_value(&old).unwrap();
    v.as_object_mut().unwrap().remove("coef_covariance").expect("key serialised");
    let back: FrozenFit = serde_json::from_value(v).unwrap();
    assert!(back.coef_covariance.is_empty());
    back.validate().unwrap();
    assert_eq!(back.content_digest, old.content_digest);
    assert_ne!(old.content_digest, fit.content_digest, "stored covariance enters the digest");
    // One ULP in a stored matrix changes the digest.
    let mut edited = fit.clone();
    let m = edited.coef_covariance[0].covariance.as_mut().unwrap();
    m[1][1] = f64::from_bits(m[1][1].to_bits() + 1);
    assert_ne!(edited.compute_digest(), fit.content_digest);
    assert!(edited.validate().unwrap_err().contains("digest"));
    // validate rejects bad shapes, asymmetry, non-finite values and the wrong count.
    let bad = |f: &dyn Fn(&mut FrozenFit)| {
        let mut x = fit.clone();
        f(&mut x);
        x.content_digest = x.compute_digest();
        x.validate().unwrap_err()
    };
    assert!(bad(&|x| {
        x.coef_covariance[0].covariance.as_mut().unwrap().pop();
    })
    .contains("k×k"));
    assert!(bad(&|x| x.coef_covariance[1].covariance.as_mut().unwrap()[0].push(0.)).contains("k×k"));
    assert!(bad(&|x| x.coef_covariance[2].covariance.as_mut().unwrap()[0][1] = f64::NAN).contains("k×k"));
    assert!(bad(&|x| x.coef_covariance[2].covariance.as_mut().unwrap()[0][1] = 1e3).contains("symmetric"));
    assert!(bad(&|x| x.coef_covariance[3].covariance.as_mut().unwrap()[1][1] = -1.).contains("negative"));
    assert!(bad(&|x| {
        x.coef_covariance.pop();
    })
    .contains("count"));
    assert!(bad(&|x| x.model.origin = FitOrigin::Relate).contains("count"));
}

#[test]
fn raw_dependency_revisions_and_legacy_coverage() {
    let (t, f) = related(500, 0.8, 5);
    for (target, predictor, lag, revised_input) in [
        (Transform::LogReturn { h: 1 }, Transform::Level, 0, 0),
        (Transform::Level, Transform::LogReturn { h: 1 }, 0, 1),
        (Transform::Level, Transform::Level, 3, 1),
        (Transform::LogReturn { h: 5 }, Transform::LogReturn { h: 5 }, 3, 1),
    ] {
        let spec = LabSpec { target, factors: vec![FactorSpec::new(F1, predictor, lag)], ..Default::default() };
        let report = studio_lab::compute(&t, &[f.clone()], &spec, Timeframe::D1, true, "unit", 0, None, 7);
        let fit = freeze_lab(&report, None, None, 0).unwrap();
        let first = fit.span_digests[revised_input].first_ts;
        assert!(first < fit.model.train_start);
        let (mut revised_t, mut revised_f) = (t.clone(), f.clone());
        let row = if revised_input == 0 { &mut revised_t } else { &mut revised_f };
        let raw = row.bars.iter_mut().find(|b| b.ts == first).unwrap();
        *raw = bar(raw.ts, raw.close + 0.01);
        let scored = run(&fit, &revised_t, &[revised_f]);
        assert!(matches!(scored.revision[revised_input].1, InputCheck::Revised { .. }));
        let mut legacy = fit.clone();
        for span in &mut legacy.span_digests { span.version = 0; }
        legacy.content_digest = legacy.compute_digest();
        let mut json = serde_json::to_value(&legacy).unwrap();
        for span in json["span_digests"].as_array_mut().unwrap() { span.as_object_mut().unwrap().remove("version"); }
        let loaded: FrozenFit = serde_json::from_value(json).unwrap();
        loaded.validate().unwrap();
        assert!(run(&loaded, &t, &[f.clone()]).revision.iter().all(|(_, c)| *c == InputCheck::PartialCoverage));
    }
}

#[test]
fn relate_raw_lookback_revision() {
    let (t, f) = related(500, 0.9, 13);
    let opts = Options { annual_periods: 252., daily: true };
    let spec = RelateSpec::new(F1, Transform::LogReturn { h: 5 });
    let report = compute_relate(&t, &f, &[], opts, Timeframe::D1, &spec, "unit", 0, None, 1).unwrap();
    let mut corr = compute_series_at_horizon(&t, &f, &[], opts, "unit", 5).unwrap();
    corr.relate = Some(Box::new(report));
    let fit = freeze_relate(&corr, Timeframe::D1, None, None, 0).unwrap();
    for input in 0..2 {
        let (mut a, mut b) = (t.clone(), f.clone());
        let row = if input == 0 { &mut a } else { &mut b };
        let first = fit.span_digests[input].first_ts;
        assert!(first < fit.model.train_start);
        let raw = row.bars.iter_mut().find(|b| b.ts == first).unwrap();
        *raw = bar(raw.ts, raw.close + 0.01);
        assert!(matches!(run(&fit, &a, &[b]).revision[input].1, InputCheck::Revised { .. }));
    }
}

#[test]
fn residual_daily_receipt_changes_do_not_change_provider_identity() {
    let (t, mut f) = level_pair(160, 51);
    f.source = "Yahoo (native provider) · CLOSED THRU 2026-01-02 · 1000 bars · observed 2026-01-03 · fallback: archive empty".into();
    let fit = manual_fit(&[&t, &f], &[1., 2.], 0., 1., 99, 20);
    f.source = "Yahoo (native provider) · CLOSED THRU 2026-01-05 · 1001 bars · observed 2026-01-06 · fallback: archive unavailable".into();
    assert_eq!(run(&fit, &t, &[f.clone()]).revision[1].1, InputCheck::Unchanged);
    f.source = f.source.replace("Yahoo", "Alpaca");
    assert!(matches!(run(&fit, &t, &[f]).revision[1].1, InputCheck::SourceChanged { .. }));
}

#[test]
fn dependency_bounds_tolerate_gaps_without_panicking() {
    use tr_core::Bar;
    let day = 86_400;
    let bars: Vec<Bar> = [0, 1, 3, 4].iter().map(|d| Bar::new(d * day, 1., 1., 1., 1., 1.)).collect();
    // Start falls on a missing day: the as-of bar before it is included.
    assert_eq!(super::dependency_bounds(&bars, 2 * day, 4 * day, true), (day, 4 * day));
    // Start before the series: clamps to the first bar.
    assert_eq!(super::dependency_bounds(&bars, -day, 3 * day, true), (0, 3 * day));
    assert_eq!(super::dependency_bounds(&[], 5, 9, true), (5, 9));
}
