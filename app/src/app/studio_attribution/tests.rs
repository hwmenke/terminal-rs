//! ATTR pane tests: stale responses, sample mode, layouts, and the request pipeline.
use super::*;
use crate::layouts::PaneSettings;
use crate::worker::Worker;

fn loaded() -> (AttrPane, Arc<FrozenFit>, Arc<ResidualReport>) {
    let (fit, rep) = fixture(400);
    let mut f = (*fit).clone();
    (f.id, f.name) = ("bench-v1".into(), "bench".into());
    let fit = Arc::new(f);
    let mut pane = AttrPane::new("bench-v1");
    (pane.token, pane.attr_token) = (41, 51);
    pane.model = Some(fit.clone());
    (pane, fit, rep)
}

fn report(fit: &FrozenFit, rep: &ResidualReport, w: &AttrWindow) -> Arc<AttributionReport> {
    Arc::new(attr::attribute(fit, rep, w))
}

#[test]
fn attr_stale_responses_ignored() {
    let (mut pane, fit, rep) = loaded();
    let digest = fit.content_digest;
    pane.scoring = true;
    // A score for an old token or another model is dropped.
    pane.accept_scored(40, digest, Ok(rep.clone()));
    pane.accept_scored(41, digest ^ 1, Ok(rep.clone()));
    assert!(pane.report.is_none() && pane.scoring);
    pane.accept_scored(41, digest, Ok(rep.clone()));
    assert!(pane.report.is_some() && !pane.scoring);
    let w = pane.window.clone();
    let a = report(&fit, &rep, &w);
    pane.attributing = true;
    // Old attr token, foreign digest, foreign score key and an old window are all dropped.
    pane.accept_attributed(50, digest, rep.key, &w, Ok(a.clone()));
    pane.accept_attributed(51, digest ^ 1, rep.key, &w, Ok(a.clone()));
    pane.accept_attributed(51, digest, rep.key ^ 1, &w, Ok(a.clone()));
    pane.accept_attributed(51, digest, rep.key, &AttrWindow::Obs { obs: 5, to: None }, Ok(a.clone()));
    assert!(pane.attribution.is_none() && pane.attributing, "stale attribution accepted");
    // A report whose own key fields do not match is refused, not shown.
    let mut wrong = (*a).clone();
    wrong.score_key ^= 1;
    pane.accept_attributed(51, digest, rep.key, &w, Ok(Arc::new(wrong)));
    assert!(pane.attribution.is_none() && pane.attr_error.is_some());
    pane.attr_error = None;
    pane.attributing = true;
    pane.accept_attributed(51, digest, rep.key, &w, Ok(a.clone()));
    assert!(pane.attribution.is_some() && !pane.attributing && pane.previous.is_none());
    // A window change allocates a fresh attr token and keeps the old report as the snapshot.
    let t = pane.attr_token;
    pane.set_window(AttrWindow::Obs { obs: 5, to: None });
    assert_ne!(pane.attr_token, t);
    assert!(pane.attribution.is_none() && pane.previous.is_some());
    // The load / score token is untouched by a window change.
    assert_eq!(pane.token, 41);
    // A refusal clears the snapshot.
    let w5 = pane.window.clone();
    let tok = pane.attr_token;
    pane.accept_attributed(tok, digest, rep.key, &w5, Err("refused".into()));
    assert!(pane.previous.is_none() && pane.attribution.is_none() && pane.attr_error.as_deref() == Some("refused"));
    // Reload keeps the snapshot, but an unavailable score must not hide behind the old numbers.
    pane.accept_attributed(tok, digest, rep.key, &w5, Ok(report(&fit, &rep, &w5)));
    assert!(pane.attribution.is_some());
    pane.reload();
    assert!(pane.previous.is_some());
    let mut un = (*rep).clone();
    un.unavailable = Some("no overlap".into());
    let tok = pane.token;
    pane.accept_scored(tok, digest, Ok(Arc::new(un)));
    assert!(pane.previous.is_none(), "unavailable score hidden behind the old snapshot");
    let (worker, _requests) = Worker::test_channel();
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(
        egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900., 900.))), ..Default::default() },
        |ui| {
            show(ui, 6, &mut pane, &worker, false);
        },
    );
    out.textures_delta.clear();
    let text: String = out
        .shapes
        .iter()
        .filter_map(|s| if let egui::Shape::Text(t) = &s.shape { Some(t.galley.job.text.clone()) } else { None })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("score unavailable"), "{text}");
    assert!(!text.contains("PREVIOUS SNAPSHOT"));
}

#[test]
fn attr_sample_mode_clears_and_requests_nothing() {
    let (mut pane, fit, rep) = loaded();
    pane.report = Some(rep.clone());
    pane.attribution = Some(report(&fit, &rep, &pane.window));
    pane.previous = pane.attribution.clone();
    pane.clear_for_sample();
    assert!(pane.report.is_none() && pane.attribution.is_none() && pane.previous.is_none());
    assert!(pane.next_request(3).is_none(), "sample mode produced a request");
    let (worker, mut requests) = Worker::test_channel();
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(
        egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900., 900.))), ..Default::default() },
        |ui| {
            show(ui, 6, &mut pane, &worker, true);
        },
    );
    out.textures_delta.clear();
    assert!(pane.sample_text.as_deref().is_some_and(|t| t.contains("no provider calls")));
    assert!(requests.try_recv().is_err(), "sample mode dispatched a request");
}

#[test]
fn attr_pipeline_one_request_per_phase() {
    let (worker, mut requests) = Worker::test_channel();
    let mut pane = AttrPane::new(String::new());
    pane.drive(3, &worker);
    pane.drive(3, &worker);
    assert!(matches!(requests.try_recv(), Ok(Request::ListStudioFits { .. })));
    assert!(requests.try_recv().is_err());
    let (mut pane, _, rep) = loaded();
    pane.model = None;
    pane.drive(3, &worker);
    pane.drive(3, &worker);
    assert!(matches!(requests.try_recv(), Ok(Request::LoadStudioFit { ref id, .. }) if id == "bench-v1"));
    assert!(requests.try_recv().is_err());
    let (mut pane, _, rep2) = loaded();
    pane.drive(3, &worker);
    pane.drive(3, &worker);
    assert!(matches!(requests.try_recv(), Ok(Request::ScoreStudioResidual { token: 41, .. })));
    assert!(requests.try_recv().is_err());
    pane.accept_scored(41, pane.model.as_ref().unwrap().content_digest, Ok(rep2));
    pane.drive(3, &worker);
    pane.drive(3, &worker);
    assert!(matches!(requests.try_recv(), Ok(Request::StudioAttribution { token: 51, .. })));
    assert!(requests.try_recv().is_err());
    let _ = rep;
}

#[test]
fn attr_layout_roundtrip_and_legacy_route() {
    let settings = PaneSettings::StudioAttr { model_id: "AAPL-v-SPY-v1".into(), window: "obs=20".into() };
    let json = serde_json::to_string(&settings).unwrap();
    let back: PaneSettings = serde_json::from_str(&json).unwrap();
    assert_eq!(back, settings);
    let PaneKind::StudioAttr(p) = PaneKind::from_settings(back) else { panic!("not an ATTR pane") };
    assert!(p.model_id == "AAPL-v-SPY-v1" && p.window == AttrWindow::Obs { obs: 20, to: None });
    assert_eq!(PaneKind::StudioAttr(p).settings(), settings);
    let partial: PaneSettings = serde_json::from_str(r#"{"StudioAttr":{}}"#).unwrap();
    let PaneKind::StudioAttr(p) = PaneKind::from_settings(partial) else { panic!() };
    assert!(p.model_id.is_empty() && p.window == AttrWindow::default());
    let legacy: PaneSettings = serde_json::from_str(r#"{"StudioRoute":{"route":"Attr"}}"#).unwrap();
    let PaneKind::StudioAttr(p) = PaneKind::from_settings(legacy) else { panic!("legacy route is not an ATTR pane") };
    assert!(p.model_id.is_empty() && p.model.is_none());
    // A hand-edited path or window never survives.
    let evil: PaneSettings =
        serde_json::from_str(r#"{"StudioAttr":{"model_id":"../../etc/passwd","window":"obs=0 bogus"}}"#).unwrap();
    let PaneKind::StudioAttr(p) = PaneKind::from_settings(evil) else { panic!() };
    assert!(p.model_id.is_empty() && p.window == AttrWindow::default());
    // A range window round-trips through its words.
    // A `YYYY-MM-DD` bound is that UTC day at 23:59:59.
    let w = AttrWindow::Range { from: 1_772_495_999, to: Some(1_774_051_199) };
    let p = AttrPane::restore("bench-v1", &w.words(true));
    assert_eq!(p.window, w);
}

#[test]
fn attr_opens_from_resid_without_rescoring() {
    let (_, fit, rep) = loaded();
    let mut pane = AttrPane::from_resid(fit.clone(), Some(rep.clone()), AttrWindow::Obs { obs: 5, to: None });
    match pane.next_request(8) {
        Some(Request::StudioAttribution { report, window, .. }) => {
            assert!(Arc::ptr_eq(&report, &rep), "the scored report was not reused");
            assert_eq!(window, AttrWindow::Obs { obs: 5, to: None });
        }
        Some(_) => panic!("expected StudioAttribution, got another request"),
        None => panic!("expected StudioAttribution, got no request"),
    }
    // A report of another model is discarded and a rescore is requested.
    let mut foreign = (*rep).clone();
    foreign.model_digest ^= 1;
    let mut pane = AttrPane::from_resid(fit, Some(Arc::new(foreign)), AttrWindow::default());
    assert!(pane.report.is_none());
    assert!(matches!(pane.next_request(8), Some(Request::ScoreStudioResidual { .. })));
}

#[test]
fn attr_blocking_is_single_flight_and_panel_selection_is_free() {
    let (mut pane, fit, rep) = loaded();
    let w = pane.window.clone();
    let a = attribute_blocking(&fit, &rep, &w).unwrap();
    let b = attribute_blocking(&fit, &rep, &w).unwrap();
    assert!(Arc::ptr_eq(&a, &b), "same key must hit the cache");
    pane.report = Some(rep.clone());
    pane.attribution = Some(a);
    let (worker, mut requests) = Worker::test_channel();
    let ctx = egui::Context::default();
    for se in [AttributionSe::Predictive, AttributionSe::Coefficient] {
        pane.ui.se = se;
        let mut out = ctx.run_ui(
            egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200., 1400.))), ..Default::default() },
            |ui| {
                show(ui, 6, &mut pane, &worker, false);
            },
        );
        out.textures_delta.clear();
    }
    assert!(requests.try_recv().is_err(), "display choices dispatched work");
}

#[test]
fn empty_library_offers_regression_lab() {
    let mut pane = AttrPane::new("");
    pane.listing = Some(Vec::new());
    let (worker, _requests) = Worker::test_channel();
    let mut out = egui::Context::default().run_ui(
        egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000., 800.))), ..Default::default() },
        |ui| { show(ui, 6, &mut pane, &worker, false); },
    );
    out.textures_delta.clear();
    let text = out.shapes.iter().filter_map(|s| match &s.shape {
        egui::Shape::Text(t) => Some(t.galley.job.text.as_str()), _ => None,
    }).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Open Regression Lab"), "{text}");
    assert!(text.contains("STUDIO lab"), "{text}");
}
