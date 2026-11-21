use super::*;
fn s(us: f64) -> Summary {
    Summary { median_us: us, p95_us: us, min_us: us, n: 9 }
}
fn b(us: f64) -> Budget {
    Budget { budget_us: us, relative: true, class: "test".into(), over_target: false }
}
#[test]
fn summary_median_p95_min_of_known_samples() {
    let r = summarize(&mut [4., 1., 3., 2.]);
    assert_eq!((r.median_us, r.p95_us, r.min_us, r.n), (2.5, 4., 1., 4));
}
#[test]
fn measure_warms_up_then_batches_fast_closures() {
    let mut calls = 0;
    let r = measure(&mut || {
        calls += 1;
        black_box(calls)
    });
    assert!(calls > r.n + 3);
    assert!((9..=201).contains(&r.n));
    assert!(r.median_us > 0.0);
}
#[test]
fn check_fails_over_budget_and_passes_under() {
    assert!(matches!(check(&s(101.), &b(100.), None, 1., 1.), Verdict::Fail(_)));
    assert_eq!(check(&s(99.), &b(100.), Some((&s(99.), 1.)), 1., 1.), Verdict::Pass);
}
#[test]
fn check_relative_needs_ratio_and_absolute_floor() {
    assert!(matches!(check(&s(160.), &b(1000.), Some((&s(100.), 1.)), 1., 1.), Verdict::Fail(_)));
    assert!(matches!(check(&s(16.), &b(1000.), Some((&s(10.), 1.)), 1., 1.), Verdict::Warn(_)));
    assert!(matches!(check(&s(130.), &b(1000.), Some((&s(100.), 1.)), 1., 1.), Verdict::Warn(_)));
}
#[test]
fn calibration_rescales_the_baseline() {
    assert_eq!(check(&s(200.), &b(1000.), Some((&s(100.), 2.)), 4., 1.), Verdict::Pass);
}
#[test]
fn slack_multiplies_the_budget() {
    assert_eq!(check(&s(150.), &b(100.), None, 1., 2.), Verdict::New);
    assert!(matches!(check(&s(1.), &b(100.), None, 1., f64::NAN), Verdict::Fail(_)));
}
#[test]
fn baseline_round_trips_and_missing_case_is_new_not_fail() {
    let mut base = Baseline::new(10.);
    base.cases.insert("test".into(), s(20.));
    let json = serde_json::to_vec(&base).unwrap();
    let decoded: Baseline = serde_json::from_slice(&json).unwrap();
    assert_eq!(decoded.cases["test"].median_us, 20.);
    assert_eq!(check(&s(1.), &b(2.), None, 10., 1.), Verdict::New);
}
#[test]
fn budgets_and_suite_ids_match_exactly() {
    let cases = suite::all(suite::Scale::Smoke);
    let mut budgets = Budgets::load().unwrap();
    validate_ids(&cases, &budgets).unwrap();
    budgets.cases.insert("stale".into(), b(1.));
    assert!(validate_ids(&cases, &budgets).is_err());
    budgets.cases.remove("stale");
    budgets.cases.remove(cases[0].id);
    assert!(validate_ids(&cases, &budgets).is_err());
}
#[test]
fn fixtures_are_deterministic_verified_and_session_aligned() {
    let a = fixtures::intraday(tr_core::Timeframe::M1, 60, 1);
    let b = fixtures::intraday(tr_core::Timeframe::M1, 60, 1);
    assert_eq!(a.len(), 23400);
    assert_eq!(tr_data::stable_hash::bars_digest(&a), tr_data::stable_hash::bars_digest(&b));
    tr_core::Bar::require_verified(&a, "test").unwrap();
    let d = fixtures::daily(6000, 3);
    tr_data::backtest_honesty::validate_daily(&d).unwrap();
    for day in a.chunks(390) {
        assert_eq!(
            day.iter().map(|b| b.ts).collect::<Vec<_>>(),
            tr_data::intraday::regular_opens(tr_core::ny::civil(day[0].ts).date(), tr_core::Timeframe::M1)
        );
    }
}
#[test]
fn every_case_does_real_work_at_smoke_scale() {
    for mut c in suite::all(suite::Scale::Smoke) {
        assert!((c.run)() > 0, "{}", c.id);
    }
}
fn run_test(f: impl FnMut() -> usize + 'static, budget: f64) -> Report {
    run_cases(
        vec![Case { id: "test", run: Box::new(f) }],
        &Budgets { schema: 1, cases: BTreeMap::from([("test".into(), b(budget))]) },
        None,
        &Opts::default(),
        1.,
    )
}
#[test]
fn gate_exits_1_on_an_injected_slow_case_and_0_otherwise() {
    let slow = run_test(
        || {
            std::thread::sleep(Duration::from_millis(2));
            1
        },
        500.,
    );
    assert_eq!(slow.exit_code(), 1);
    assert_eq!(run_test(|| 1, 1000.).exit_code(), 0);
}
#[test]
fn zero_work_token_is_a_harness_error() {
    assert_eq!(run_test(|| 0, 1000.).exit_code(), 2);
}
#[test]
fn panic_is_a_harness_error() {
    assert_eq!(run_test(|| panic!("injected"), 1000.).exit_code(), 2);
}
#[test]
fn fixtures_never_ship_in_the_app() {
    for file in ["app/Cargo.toml", "crates/data/Cargo.toml", "crates/core/Cargo.toml", "crates/chart/Cargo.toml"] {
        let manifest = std::fs::read_to_string(root().join(file)).unwrap();
        let mut section = "";
        for line in manifest.lines() {
            let l = line.trim();
            if l.starts_with('[') {
                section = l;
            }
            if l.starts_with("tr-perf") {
                assert_eq!(section, "[dev-dependencies]", "fixtures in shipped dependency: {file}");
            }
        }
    }
}
