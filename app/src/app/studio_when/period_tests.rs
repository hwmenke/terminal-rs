//! Period controls: cache identity, stale responses, copied command, saved-layout compatibility.
use super::*;
use tr_core::{Bar, Provenance};
use tr_data::source_receipt::SourceReceipt;
use tr_data::studio_when::WhenPeriod;

/// Fixture only: verified-tagged daily bars on real NYSE sessions (never reaches app display).
fn input(sessions: usize) -> tr_data::studio_canvas::CanvasInput {
    let mut day = chrono::NaiveDate::from_ymd_opt(2012, 1, 3).unwrap();
    let mut bars = vec![];
    for k in 0..sessions {
        let c = 100. + 6. * ((k as f64) * 0.09).sin() + (k % 4) as f64 * 0.15;
        bars.push(Bar {
            ts: day.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(),
            open: c,
            high: c * 1.002,
            low: c * 0.998,
            close: c,
            volume: 1_000.,
            provenance: Provenance::Real,
        });
        day = tr_data::history::calendar::next_session(day);
    }
    tr_data::studio_canvas::CanvasInput {
        symbol: "AAPL US Equity".into(),
        tf: Timeframe::D1,
        observed_at: Some(bars.last().unwrap().ts + 3 * 86_400),
        stitch: None,
        source: "period fixture".into(),
        bars: Arc::new(bars),
    }
}

#[test]
fn period_is_part_of_the_cache_identity() {
    let inp = input(3_000);
    let r5 = cached_report_period(&inp, "close>SMA(50)", 0., WhenPeriod::Years(5), None).unwrap();
    let r10 = cached_report_period(&inp, "close>SMA(50)", 0., WhenPeriod::Years(10), None).unwrap();
    assert_ne!(r5.report.key, r10.report.key);
    assert!(r5.report.sessions < r10.report.sessions);
    // Same period resolves to the very same cached Arc (no recompute).
    let again = cached_report_period(&inp, "close>SMA(50)", 0., WhenPeriod::Years(5), None).unwrap();
    assert!(Arc::ptr_eq(&r5, &again));
}

#[test]
fn stale_period_response_cannot_replace_new_result() {
    let inp = input(3_000);
    let r10 = cached_report_period(&inp, "close>SMA(50)", 0., WhenPeriod::Years(10), None).unwrap();
    let r5 = cached_report_period(&inp, "close>SMA(50)", 0., WhenPeriod::Years(5), None).unwrap();
    let mut p = WhenPane::new("AAPL US Equity", Timeframe::D1, "close>SMA(50)", 0., 6);
    assert_eq!(p.period, WhenPeriod::Years(10));
    p.loading = true;
    let first_generation = p.generation;
    // The user switches to 5Y while the 10Y request is in flight.
    p.set_period(WhenPeriod::Years(5));
    assert_ne!(p.generation, first_generation);
    p.loading = true;
    p.accept(first_generation, Ok(r10.clone()));
    assert!(p.report.is_none() && p.loading, "late 10Y response is ignored by generation");
    // Even with the current generation, a report for another period is refused.
    p.accept(p.generation, Ok(r10));
    assert!(p.report.is_none() && p.error.is_some());
    p.loading = true;
    p.error = None;
    p.accept(p.generation, Ok(r5));
    assert!(p.report.is_some() && p.error.is_none());
    assert_eq!(p.report.as_ref().unwrap().report.window.as_ref().unwrap().period, WhenPeriod::Years(5));
}

#[test]
fn receipt_is_carried_and_window_is_disclosed() {
    let inp = input(3_000);
    let mut receipt = SourceReceipt::new("yahoo", 3_000, inp.bars.last().map(|b| b.ts));
    receipt.attempts = vec!["Local research archive: no history".into()];
    let r = cached_report_period(&inp, "close>SMA(20)", 0., WhenPeriod::Years(10), Some(receipt.clone())).unwrap();
    assert_eq!(r.source_receipts, vec![receipt]);
    let line = window_line(r.report.window.as_ref().unwrap(), r.report.sessions);
    assert!(line.starts_with("Analysis period 10Y") && line.contains("warm-up bars not scored"), "{line}");
}

#[test]
fn period_in_copied_command_and_saved_layouts() {
    use super::super::studio_hub::StudioInputs;
    let cmd = tr_data::studio_when::parse_command("period=5y RSI(14)<30").unwrap();
    let pane = crate::app::studio_hub::when_studio("AAPL US Equity".into(), cmd);
    let command = pane.command();
    assert!(command.contains("period=5y") && command.contains("lens=when"), "{command}");
    // Older saved layouts have no `when_period`: they load with the default 10Y.
    let old: StudioInputs = serde_json::from_str(r#"{"condition":"RSI(14)<30","when_threshold":10.0}"#).unwrap();
    assert_eq!(old.when_period_value(), WhenPeriod::Years(10));
    let saved = StudioInputs { when_period: WhenPeriod::Since(chrono::NaiveDate::from_ymd_opt(2015, 1, 2).unwrap()).token(), ..old };
    let back: StudioInputs = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
    assert_eq!(back.when_period_value(), WhenPeriod::Since(chrono::NaiveDate::from_ymd_opt(2015, 1, 2).unwrap()));
}

#[test]
fn over_limit_period_error_is_not_red_failure_text() {
    // 21,500 daily sessions: the whole-history request is refused with the stable prefix, 10Y opens.
    let mut day = chrono::NaiveDate::from_ymd_opt(1942, 1, 2).unwrap();
    let mut bars = vec![];
    for k in 0..21_500usize {
        let c = 100. + 6. * ((k as f64) * 0.09).sin() + k as f64 * 0.001;
        bars.push(Bar {
            ts: day.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(),
            open: c,
            high: c * 1.002,
            low: c * 0.998,
            close: c,
            volume: 1_000.,
            provenance: Provenance::Real,
        });
        day = tr_data::history::calendar::next_session(day);
    }
    let mut inp = input(10);
    inp.observed_at = Some(bars.last().unwrap().ts + 3 * 86_400);
    inp.bars = Arc::new(bars);
    inp.source = "spx-like fixture".into();
    let err = cached_report_period(&inp, "RSI(14)<30", 10., WhenPeriod::Years(100), None).err().unwrap();
    assert!(err.starts_with(tr_data::studio_when::PERIOD_LIMIT_PREFIX), "{err}");
    let ok = cached_report_period(&inp, "RSI(14)<30", 10., WhenPeriod::default(), None).unwrap();
    assert!(ok.report.sessions > 2_000 && ok.report.sessions < 3_000);
}
