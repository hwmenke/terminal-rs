//! Lab pane tests: sample mode, stale responses, display-only selections, layouts.
use super::*;
use crate::layouts::PaneSettings;
use crate::worker::Worker;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

fn pane_with_report() -> (LabPane, Arc<LabReport>) {
    let report = Arc::new(fixture_report(400));
    assert!(report.unavailable.is_none() && report.fit.is_some(), "{:?}", report.unavailable);
    let mut pane = LabPane::new("AAPL US Equity", Timeframe::D1, report.spec.clone());
    pane.token = 41;
    pane.loading = true;
    pane.accept(41, &report.spec.clone(), Ok(report.clone()));
    assert!(pane.report.is_some());
    (pane, report)
}

fn frame(ctx: &egui::Context, pane: &mut LabPane, worker: &Worker, sample: bool) {
    let mut out = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440., 1200.))),
            ..Default::default()
        },
        |ui| {
            show(ui, 5, pane, worker, sample);
        },
    );
    out.textures_delta.clear();
}

#[tokio::test]
async fn lab_sample_mode_zero_provider_calls() {
    let history = tr_data::history::Service::new(
        tr_data::history::Archive::memory().unwrap(),
        Arc::new(NoNetwork),
        Default::default(),
    );
    let bridge = tr_data::whats_news::Config { enabled: false, ..Default::default() }.bridge();
    let calls = AtomicUsize::new(0);
    let spec = LabSpec::default_for("AAPL");
    let err = research(&CorrCache::default(), false, None, &history, &bridge, true, "AAPL", Timeframe::M30, &spec, |_, _| {
        calls.fetch_add(1, SeqCst);
        async { anyhow::bail!("must not be called") }
    })
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("sample"), "{err:#}");
    assert_eq!(calls.load(SeqCst), 0, "sample mode made provider calls");
}

#[test]
fn lab_sample_mode_clears_numbers_and_requests_nothing() {
    // The pane clears its numbers and its snapshot.
    let (mut pane, report) = pane_with_report();
    pane.previous = Some(report);
    pane.clear_for_sample();
    assert!(pane.report.is_none() && pane.previous.is_none() && !pane.loading);
    let (worker, mut requests) = Worker::test_channel();
    frame(&egui::Context::default(), &mut pane, &worker, true);
    assert!(pane.report.is_none() && pane.previous.is_none());
    assert!(requests.try_recv().is_err(), "sample mode dispatched a request");
}

struct NoNetwork;
#[async_trait::async_trait]
impl tr_data::history::DailyHistory for NoNetwork {
    async fn daily(
        &self,
        _: &str,
        _: Option<chrono::NaiveDate>,
        _: chrono::NaiveDate,
    ) -> Result<Vec<Bar>, tr_data::history::Failure> {
        panic!("Lab sample-mode test must never download daily bars");
    }
}

#[test]
fn lab_stale_response_ignored() {
    let (mut pane, report) = pane_with_report();
    pane.report = None;
    pane.token = 77;
    pane.loading = true;
    // An orphaned token is ignored.
    pane.accept(76, &report.spec.clone(), Ok(report.clone()));
    assert!(pane.report.is_none() && pane.loading);
    // The matching token with a different model is ignored.
    let mut other = report.spec.clone();
    other.factors.pop();
    pane.accept(77, &other, Ok(report.clone()));
    assert!(pane.report.is_none() && pane.loading);
    // A different displayed covariance is not a different model.
    let mut shown = report.spec.clone();
    shown.covariance = tr_data::studio_inference::CovarianceMethod::HC3;
    pane.accept(77, &shown, Ok(report.clone()));
    assert!(pane.report.is_some() && !pane.loading);
    // A refusal clears the report and any snapshot.
    pane.token = 78;
    pane.previous = pane.report.clone();
    pane.accept(78, &report.spec.clone(), Err("refused".into()));
    assert!(pane.report.is_none() && pane.previous.is_none() && pane.error.as_deref() == Some("refused"));
    // A report for another symbol is ignored.
    pane.token = 79;
    pane.error = None;
    pane.loading = true;
    pane.symbol = "MSFT US Equity".into();
    pane.accept(79, &report.spec.clone(), Ok(report.clone()));
    assert!(pane.report.is_none());
}

#[test]
fn lab_apply_keeps_previous_snapshot_until_success() {
    let (mut pane, report) = pane_with_report();
    pane.draft.holdout_pct = 30;
    pane.apply().unwrap();
    assert!(pane.report.is_none() && pane.previous.as_ref().is_some_and(|p| Arc::ptr_eq(p, &report)));
    // An invalid draft is refused and leaves the running model alone.
    let before = pane.spec.clone();
    pane.draft.factors.clear();
    pane.draft_syms.clear();
    assert!(pane.apply().is_err());
    assert_eq!(pane.spec, before);
    // Apply of an unchanged model keeps nothing stale around.
    let (mut pane, _) = pane_with_report();
    pane.apply().unwrap();
    assert!(pane.previous.is_none());
}

#[test]
fn lab_selection_does_not_refit() {
    let (mut pane, report) = pane_with_report();
    let (worker, mut requests) = Worker::test_channel();
    let ctx = egui::Context::default();
    frame(&ctx, &mut pane, &worker, false);
    assert!(requests.try_recv().is_err(), "an accepted report must not request");
    for (tab, cov) in [(1, 0), (2, 1), (0, 2), (1, 3)] {
        pane.ui.diag_tab = tab;
        pane.ui.shown_cov = cov;
        frame(&ctx, &mut pane, &worker, false);
    }
    assert!(requests.try_recv().is_err(), "tab / covariance selection dispatched work");
    assert!(Arc::ptr_eq(pane.report.as_ref().unwrap(), &report));
    // Every chip has its own prepared inference block.
    assert_eq!(report.inference.len(), 4);
}

#[test]
fn lab_layout_round_trip_and_legacy_route() {
    let (pane, _) = pane_with_report();
    let settings = PaneSettings::StudioLab { symbol: pane.symbol.clone(), tf: Timeframe::D1, spec: pane.spec.clone() };
    let json = serde_json::to_string(&settings).unwrap();
    assert!(!json.contains("points") && !json.contains("token"));
    let back: PaneSettings = serde_json::from_str(&json).unwrap();
    assert_eq!(back, settings);
    let PaneKind::StudioLab(restored) = PaneKind::from_settings(back) else { panic!("not a Lab") };
    assert_eq!(restored.spec, pane.spec);
    // Partial JSON defaults every field.
    let partial: PaneSettings = serde_json::from_str(r#"{"StudioLab":{}}"#).unwrap();
    assert!(matches!(PaneKind::from_settings(partial), PaneKind::StudioLab(_)));
    // The legacy unavailable-route layout restores a default Lab pane.
    let legacy: PaneSettings = serde_json::from_str(r#"{"StudioRoute":{"route":"Lab"}}"#).unwrap();
    let PaneKind::StudioLab(p) = PaneKind::from_settings(legacy) else { panic!("legacy route is not a Lab") };
    assert!(p.symbol.is_empty() && p.report.is_none());
    // A hand-edited unsupported horizon / lag / holdout is normalised on restore.
    let mut bad = pane.spec.clone();
    bad.target = Transform::LogReturn { h: 3 };
    bad.factors[0].lag = 9999;
    bad.holdout_pct = 90;
    let p = LabPane::new("AAPL US Equity", Timeframe::D1, bad);
    assert_eq!(p.spec.target, Transform::LogReturn { h: 1 });
    assert_eq!((p.spec.factors[0].lag, p.spec.holdout_pct), (MAX_LAG, 40));
}

#[test]
fn lab_edit_words_reach_a_focused_lab() {
    let (mut pane, _) = pane_with_report();
    let args = LabArgs::parse("remove=F6 factor=TLT transform=change h=5 lag=1").unwrap();
    assert!(args.is_edit_only());
    let mut next = pane.draft.clone();
    args.apply(&mut next).unwrap();
    let tlt = next.factors.iter().find(|f| f.symbol == tr_data::security("TLT").key()).unwrap();
    assert_eq!((tlt.transform, tlt.lag), (Transform::Change { h: 5 }, 1));
    pane.set_spec(next.clone());
    assert_eq!(pane.spec.key_string(), next.key_string());
    assert!(pane.report.is_none() && pane.previous.is_some());
}

#[test]
fn save_residual_explains_each_disabled_state() {
    let (mut pane, _) = pane_with_report();
    assert!(pane.save_disabled_reason(false).is_none());
    assert!(pane.save_disabled_reason(true).unwrap().contains("sample mode"));
    pane.save.pending = Some(42);
    assert!(pane.save_disabled_reason(false).unwrap().contains("wait"));
    pane.save.pending = None;
    pane.report = None;
    let reason = pane.save_disabled_reason(false).unwrap();
    assert!(reason.contains("successful verified-data fit") && reason.contains("Reload"));
}
