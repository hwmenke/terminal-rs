use serde_json::Value;
use tr_data::whats_news::scan_pack::{VcpMetrics, measure_breakout, measure_vcp};

#[test]
fn python_vcp_lens_fixtures() {
    let source = include_str!("fixtures/scan_pack_vcp.json");
    assert!(source.len() < 15_000);
    let cases: Value = serde_json::from_str(source).unwrap();
    for case in cases.as_array().unwrap() {
        let expand = |key: &str| -> Vec<f64> {
            case[key].as_array().unwrap().iter().flat_map(|run| {
                let v = if run[1] == "nan" { f64::NAN } else { run[1].as_f64().unwrap() };
                std::iter::repeat_n(v, run[0].as_u64().unwrap() as usize)
            }).collect()
        };
        let (close, high, low) = (expand("close"), expand("high"), expand("low"));
        let breakout = measure_breakout(&close, &high, &[]);
        let expected: VcpMetrics = serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(measure_vcp(&close, &high, &low, &breakout), expected, "{}", case["name"]);
    }
}

#[test]
fn vcp_setup_filters_on_proxy() {
    use tr_core::{Bar, Provenance};
    use tr_data::whats_news::{SETUPS, scan_histories};
    assert!(SETUPS.contains(&"VCP_LENS"));
    let histories = [("MATCH", 99.), ("MISS", 70.)].into_iter().map(|(symbol, close)| {
        (symbol.into(), (0..60).map(|i| {
            let width = if i < 40 { 20. } else { 2. };
            Bar::new(1_700_000_000 + i * 86400, close, 100. + width/2., (100. - width/2.).min(close - 0.5), close, 100.).with_provenance(Provenance::Real)
        }).collect())
    }).collect();
    let scan = scan_histories("VCP_LENS", histories, Provenance::Real).unwrap();
    assert_eq!(scan.scanned, 2);
    assert_eq!(scan.results.len(), 1);
    assert_eq!(scan.results[0].symbol, "MATCH");
    assert!(scan.results[0].metrics.as_ref().unwrap().vcp_lens.vcp_proxy);
}
