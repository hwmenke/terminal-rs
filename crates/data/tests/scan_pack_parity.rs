use serde_json::Value;
use tr_data::whats_news::scan_pack::{self as sp, Breadth, MaMetrics};

fn fixtures() -> Value {
    serde_json::from_str(include_str!("fixtures/scan_pack.json")).unwrap()
}
fn closes(v: &Value) -> Vec<f64> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|v| match v.as_str() {
            Some("nan") => f64::NAN,
            Some("inf") => f64::INFINITY,
            Some("-inf") => f64::NEG_INFINITY,
            _ => v.as_f64().unwrap(),
        })
        .collect()
}
#[test]
fn python_ma_fixtures() {
    for case in fixtures()["cases"].as_array().unwrap() {
        let c = closes(&case["close"]);
        let expected: MaMetrics = serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(sp::measure(&c), expected, "{}", case["name"]);
        assert_eq!(expected.matches_ma(), expected.stacked_ma || expected.pullback_rising_ma);
    }
}
#[test]
fn python_breadth_fixtures() {
    let fixture = fixtures();
    for group in fixture["breadth"].as_array().unwrap() {
        let histories: Vec<_> = group["names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| {
                let case = fixture["cases"].as_array().unwrap().iter().find(|c| c["name"] == *name).unwrap();
                closes(&case["close"])
            })
            .collect();
        let expected: Breadth = serde_json::from_value(group["expected"].clone()).unwrap();
        assert_eq!(sp::breadth(&histories), expected, "{}", group["names"]);
    }
}
#[test]
fn helper_boundaries_and_missing_values() {
    assert!(sp::in_band(Some(sp::PULLBACK_LO)));
    assert!(sp::in_band(Some(sp::PULLBACK_HI)));
    assert!(!sp::in_band(Some(f64::NAN)));
    assert!(!sp::in_band(None));
    assert_eq!(sp::rising(Some(1.), Some(1.)), Some(false));
    assert_eq!(sp::rising(Some(f64::INFINITY), Some(1.)), None);
    assert_eq!(sp::vs(Some(1.), Some(0.)), None);
    assert_eq!(sp::sma_prev(&[1.; 20], 20), None);
}

#[test]
fn ma_scan_matches_and_sorts_use_daily_metrics() {
    use tr_core::{Bar, Provenance};
    use tr_data::whats_news::{ScanFilter, Sort, scan_histories};
    let fixture = fixtures();
    let histories: Vec<_> = ["up_220", "pullback20", "down"]
        .into_iter()
        .map(|name| {
            let case = fixture["cases"].as_array().unwrap().iter().find(|c| c["name"] == name).unwrap();
            let bars = closes(&case["close"])
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    Bar::new(1_700_000_000 + i as i64 * 86400, *c, c + 1., c - 1., *c, 1000.)
                        .with_provenance(Provenance::Real)
                })
                .collect();
            (name.to_string(), bars)
        })
        .collect();
    let breadth = sp::history_breadth(&histories);
    assert_eq!(breadth.n, 3);
    let scan = scan_histories("MA", histories, Provenance::Real).unwrap();
    assert_eq!(scan.scanned, 3);
    assert_eq!(scan.results.len(), 2);
    let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 31).unwrap();
    for (sort, first) in [(Sort::StackedMa, "up_220"), (Sort::PullbackMa, "pullback20")] {
        let filter = ScanFilter { include_stale: true, sort, ..Default::default() };
        assert_eq!(filter.rows(&scan, "", date)[0].symbol, first);
    }
}
