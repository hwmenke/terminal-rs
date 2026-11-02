use serde_json::Value;
use tr_data::whats_news::scan_pack::{OneilMetrics, measure_oneil};

#[test]
fn python_oneil_lens_fixtures() {
    let fixture = include_str!("fixtures/scan_pack_oneil.json");
    assert!(fixture.len() < 10_000);
    let cases: Value = serde_json::from_str(fixture).unwrap();
    for case in cases.as_array().unwrap() {
        let expand = |key: &str| -> Vec<f64> {
            case[key].as_array().unwrap().iter().flat_map(|run| {
                let value = if run[1] == "nan" { f64::NAN } else { run[1].as_f64().unwrap() };
                std::iter::repeat_n(value, run[0].as_u64().unwrap() as usize)
            }).collect()
        };
        let close = expand("close");
        let spy = (!case["spy"].is_null()).then(|| expand("spy"));
        let expected: OneilMetrics = serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(measure_oneil(&close, spy.as_deref()), expected, "{}", case["name"]);
    }
}

#[test]
fn oneil_scan_and_benchmark_use_positional_closes() {
    use tr_core::{Bar, Provenance};
    use tr_data::whats_news::{SETUPS, apply_spy_benchmark, scan_histories};
    assert!(SETUPS.contains(&"ONEIL_LENS"));
    let bars = |last, offset: i64| (0..65).map(|i| {
        let close = if i == 64 { last } else { 100. };
        Bar::new(1_700_000_000 + (i + offset) * 86400, close, close, close, close, 100.)
            .with_provenance(Provenance::Real)
    }).collect::<Vec<_>>();
    let spy = bars(110., 100);
    let histories = vec![("WIN".into(), bars(120., 0)), ("LOSE".into(), bars(105., 0)), ("SPY".into(), spy.clone())];
    let mut scan = scan_histories("ONEIL_LENS", histories.clone(), Provenance::Real).unwrap();
    assert_eq!(scan.results.len(), 1);
    assert_eq!(scan.results[0].symbol, "WIN");
    let expected = OneilMetrics { rs_spy_pct: Some(9.09), oneil_rs: true };
    assert_eq!(scan.results[0].metrics.as_ref().unwrap().oneil_lens, expected);
    assert_eq!(scan.results[0].metrics.as_ref().unwrap().rs_spy_63d, None);
    apply_spy_benchmark(&mut scan, &histories, Some(&spy)).unwrap();
    assert_eq!(scan.results[0].metrics.as_ref().unwrap().oneil_lens, expected);
    apply_spy_benchmark(&mut scan, &histories, None).unwrap();
    assert_eq!(scan.results[0].metrics.as_ref().unwrap().oneil_lens, OneilMetrics::default());
}
