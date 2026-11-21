use serde_json::Value;
use tr_data::whats_news::scan_pack::{RsiMetrics, measure_rsi};

#[test]
fn python_rsi_lens_fixtures() {
    let cases: Value = serde_json::from_str(include_str!("fixtures/scan_pack_rsi.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let close: Vec<_> = case["close"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| if v == "nan" { f64::NAN } else { v.as_f64().unwrap() })
            .collect();
        let expected: RsiMetrics = serde_json::from_value(case["expected"].clone()).unwrap();
        let actual = measure_rsi(&close);
        assert_eq!(actual, expected, "{}", case["name"]);
        assert_eq!(actual.matches_rsi(), case["match"].as_bool().unwrap(), "{}", case["name"]);
    }
}

#[test]
fn rsi_lens_setup_uses_separate_daily_metrics() {
    use tr_core::{Bar, Provenance};
    use tr_data::whats_news::scan_histories;
    let histories = [
        ("UP", (0..32).map(|i| 80. + i as f64).collect::<Vec<_>>()),
        ("RISING", (0..30).map(|i| 120. - i as f64).chain([93., 95.]).collect()),
        ("NEUTRAL", (0..40).map(|i| 100. + i as f64 * 0.1 + if i % 2 == 0 { 2. } else { -2. }).collect()),
    ]
    .into_iter()
    .map(|(name, closes)| {
        (
            name.into(),
            closes
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    Bar::new(1_700_000_000 + i as i64 * 86400, *c, c + 1., c - 1., *c, 1000.)
                        .with_provenance(Provenance::Real)
                })
                .collect(),
        )
    })
    .collect();
    let scan = scan_histories("RSI_LENS", histories, Provenance::Real).unwrap();
    assert_eq!(scan.scanned, 3);
    assert_eq!(scan.results.len(), 2);
    assert!(scan.results.iter().all(|r| r.metrics.as_ref().unwrap().rsi_lens.matches_rsi()));
}
