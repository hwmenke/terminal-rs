use serde_json::Value;
use tr_data::whats_news::scan_pack::{FragileMetrics, measure_fragile, window_pack, reading};

#[test]
fn python_fragile_lens_fixtures() {
    let source = include_str!("fixtures/scan_pack_fragile.json");
    assert!(source.len() < 15_000);
    let cases: Value = serde_json::from_str(source).unwrap();
    for case in cases.as_array().unwrap() {
        let close: Vec<f64> = case["close"].as_array().unwrap().iter().flat_map(|run| {
            let value = match run[1].as_str() {
                Some("nan") => f64::NAN, Some("inf") => f64::INFINITY,
                Some("-inf") => f64::NEG_INFINITY, _ => run[1].as_f64().unwrap(),
            };
            std::iter::repeat_n(value, run[0].as_u64().unwrap() as usize)
        }).collect();
        let expected: FragileMetrics = serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(measure_fragile(&close), expected, "{}", case["name"]);
        for (i, n) in [65, 130].into_iter().enumerate() {
            let actual = window_pack(&close, n);
            let expected = &case["windows"][i];
            assert_eq!(actual.h, expected["h"].as_f64(), "{} H{n}", case["name"]);
        }
    }
}

#[test]
fn fragile_reading_boundaries() {
    assert_eq!(reading(Some(1.4), Some(5.0)), Some("orderly"));
    assert_eq!(reading(Some(1.4), Some(-5.001)), Some("FRAGILE"));
    for (d, label) in [(1.5, "orderly"), (1.6, "near-random"), (1.7, "choppy"), (1.85, "max chop")] {
        assert_eq!(reading(Some(d), None), Some(label));
    }
    assert_eq!(reading(Some(f64::NAN), Some(10.)), None);
}

#[test]
fn fragile_setup_keeps_only_fragile_symbols() {
    use tr_core::{Bar, Provenance};
    use tr_data::whats_news::{SETUPS, scan_histories};
    assert_eq!(SETUPS.last(), Some(&"FRAGILE_LENS"));
    let histories = [("MATCH", true), ("MISS", false)].into_iter().map(|(symbol, trend)| {
        (symbol.into(), (0..130).map(|i| {
            let close = if trend { 100. + (i / 10) as f64 * 4. } else { 100. };
            Bar::new(1_700_000_000 + i * 86400, close, close + 1., close - 1., close, 100.).with_provenance(Provenance::Real)
        }).collect())
    }).collect();
    let scan = scan_histories("FRAGILE_LENS", histories, Provenance::Real).unwrap();
    assert_eq!(scan.scanned, 2);
    assert_eq!(scan.results.len(), 1);
    assert_eq!(scan.results[0].symbol, "MATCH");
    assert!(scan.results[0].metrics.as_ref().unwrap().fragile_lens.fragile);
}
