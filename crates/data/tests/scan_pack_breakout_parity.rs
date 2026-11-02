use serde_json::Value;
use tr_data::whats_news::scan_pack::{BreakoutMetrics, measure_breakout};

#[test]
fn python_breakout_lens_fixtures() {
    let cases: Value = serde_json::from_str(include_str!("fixtures/scan_pack_breakout.json")).unwrap();
    assert!(include_str!("fixtures/scan_pack_breakout.json").len() < 15_000);
    for case in cases.as_array().unwrap() {
        let expand = |key: &str| -> Vec<f64> {
            case[key].as_array().unwrap().iter().flat_map(|run| {
                let value = if run[1] == "nan" { f64::NAN } else { run[1].as_f64().unwrap() };
                std::iter::repeat_n(value, run[0].as_u64().unwrap() as usize)
            }).collect()
        };
        let expected: BreakoutMetrics = serde_json::from_value(case["expected"].clone()).unwrap();
        let actual = measure_breakout(&expand("close"), &expand("high"), &expand("volume"));
        assert_eq!(actual, expected, "{}", case["name"]);
        assert_eq!(actual.matches_breakout(), case["match"].as_bool().unwrap(), "{}", case["name"]);
    }
}

#[test]
fn breakout_lens_setup_is_appended_and_uses_its_own_metrics() {
    use tr_core::{Bar, Provenance};
    use tr_data::whats_news::{SETUPS, scan_histories};
    assert!(SETUPS.contains(&"BREAKOUT_LENS"));
    assert_eq!(SETUPS.last(), Some(&"QULLA_LENS"));
    let histories = [("MATCH", 96.), ("MISS", 90.)].into_iter().map(|(symbol, close)| {
        (symbol.into(), (0..60).map(|i| Bar::new(1_700_000_000 + i * 86400, close, 100., 80., close, 100.).with_provenance(Provenance::Real)).collect())
    }).collect();
    let scan = scan_histories("BREAKOUT_LENS", histories, Provenance::Real).unwrap();
    assert_eq!(scan.scanned, 2);
    assert_eq!(scan.results.len(), 1);
    assert_eq!(scan.results[0].symbol, "MATCH");
    assert!(scan.results[0].metrics.as_ref().unwrap().breakout_lens.matches_breakout());
}

#[test]
fn scan_breadth_counts_full_loaded_universe_and_uses_latest_bar_date() {
    use tr_core::Bar;
    use tr_data::whats_news::scan_pack::ScanBreadth;
    let histories = vec![
        ("SHORT".into(), vec![Bar::new(1_793_318_400, 100., 100., 100., 100., 100.)]),
        ("EMPTY".into(), vec![]),
        ("OLDER".into(), vec![Bar::new(1_793_232_000, 100., 100., 100., 100., 100.)]),
    ];
    let cached = ScanBreadth::from_histories(&histories);
    assert_eq!(cached.symbols, 3);
    assert_eq!(cached.breadth.n, 2);
    assert_eq!(cached.as_of, chrono::DateTime::from_timestamp(1_793_318_400, 0).map(|d| d.date_naive()));
    assert_eq!(ScanBreadth::from_histories(&[]), ScanBreadth::default());
}
