use serde_json::Value;
use tr_data::whats_news::scan_pack::{QullaMetrics, measure_breakout, measure_qulla};

#[test]
fn python_qulla_lens_fixtures() {
    let fixture = include_str!("fixtures/scan_pack_qulla.json");
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
        let open = (!case["open"].is_null()).then(|| expand("open"));
        let breakout = measure_breakout(&close, &expand("high"), &expand("volume"));
        let expected: QullaMetrics = serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(measure_qulla(&close, open.as_deref(), &breakout), expected, "{}", case["name"]);
    }
}

#[test]
fn qulla_is_appended_and_scan_uses_lens_metrics() {
    use tr_core::{Bar, Provenance};
    use tr_data::whats_news::{SETUPS, scan_histories};
    assert!(SETUPS.contains(&"QULLA_LENS"));
    let histories = [("NEAR", 96., 100.), ("VOL", 90., 150.), ("MISS", 90., 100.)].into_iter().map(|(symbol, close, volume)| {
        (symbol.into(), (0..60).map(|i| Bar::new(1_700_000_000 + i * 86400, close, 100., 80., close, if i == 59 { volume } else { 100. }).with_provenance(Provenance::Real)).collect())
    }).collect();
    let scan = scan_histories("QULLA_LENS", histories, Provenance::Real).unwrap();
    assert_eq!(scan.results.len(), 2);
    assert!(scan.results.iter().all(|r| r.metrics.as_ref().unwrap().qulla_lens.qulla));
}
