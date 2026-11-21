//! RESID pane tests: stale responses, sample mode, layouts, and the request pipeline.
use super::*;
use crate::layouts::PaneSettings;
use crate::worker::Worker;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

fn loaded_pane() -> (ResidPane, Arc<FrozenFit>, Arc<ResidualReport>, Arc<ResidualChecks>) {
    let (fit, rep, chk) = fixture(400);
    let mut f = (*fit).clone();
    (f.id, f.name) = ("bench-v1".into(), "bench".into());
    let fit = Arc::new(f);
    let mut pane = ResidPane::new("bench-v1");
    pane.token = 41;
    pane.model = Some(fit.clone());
    (pane, fit, rep, chk)
}

fn frame(ctx: &egui::Context, pane: &mut ResidPane, worker: &Worker, sample: bool) {
    let mut out = ctx.run_ui(
        egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440., 1400.))), ..Default::default() },
        |ui| {
            show(ui, 6, pane, worker, sample);
        },
    );
    out.textures_delta.clear();
}

#[test]
fn resid_stale_responses_ignored() {
    let (mut pane, fit, rep, chk) = loaded_pane();
    let digest = fit.content_digest;
    pane.scoring = true;
    // Scored: an old token, a different model digest.
    pane.accept_scored(40, digest, Ok(rep.clone()));
    assert!(pane.report.is_none() && pane.scoring, "old token accepted");
    pane.accept_scored(41, digest ^ 1, Ok(rep.clone()));
    assert!(pane.report.is_none() && pane.scoring, "foreign model digest accepted");
    pane.accept_scored(41, digest, Ok(rep.clone()));
    assert!(pane.report.is_some() && !pane.scoring);
    // Checked: an old token, another model, and checks for an old score key.
    pane.checking = true;
    pane.accept_checked(40, digest, rep.key, Ok(chk.clone()));
    pane.accept_checked(41, digest ^ 1, rep.key, Ok(chk.clone()));
    pane.accept_checked(41, digest, rep.key ^ 1, Ok(chk.clone()));
    assert!(pane.checks.is_none() && pane.checking, "stale checks accepted");
    pane.accept_checked(41, digest, rep.key, Ok(chk.clone()));
    assert!(pane.checks.is_some() && !pane.checking);
    // Loaded: another token or another id is ignored.
    let mut p2 = ResidPane::new("bench-v1");
    p2.token = 7;
    p2.loading_model = true;
    p2.accept_loaded(6, "bench-v1", Ok(fit.clone()));
    p2.accept_loaded(7, "other-v1", Ok(fit.clone()));
    assert!(p2.model.is_none() && p2.loading_model);
    p2.accept_loaded(7, "bench-v1", Ok(fit.clone()));
    assert!(p2.model.is_some() && !p2.loading_model);
    // A refusal clears the report and the snapshot; the model stays.
    pane.token = 42;
    pane.previous = pane.report.clone();
    pane.accept_scored(42, digest, Err("refused".into()));
    assert!(pane.report.is_none() && pane.previous.is_none() && pane.error.as_deref() == Some("refused"));
    assert!(pane.model.is_some());
    // An unverified report is refused.
    pane.token = 43;
    let mut sample = (*rep).clone();
    sample.provenance = tr_core::Provenance::Sample;
    pane.accept_scored(43, digest, Ok(Arc::new(sample)));
    assert!(pane.report.is_none() && pane.error.is_some());
    // Unavailable model: kind is kept and no model is shown.
    let mut p3 = ResidPane::new("gone-v1");
    p3.token = 9;
    p3.accept_loaded(
        9,
        "gone-v1",
        Err(ModelUnavailable {
            kind: crate::studio_models::UnavailableKind::Missing,
            id: "gone-v1".into(),
            message: "no such saved model".into(),
        }),
    );
    assert!(p3.model.is_none() && p3.unavailable.is_some());
}

#[test]
fn resid_reload_and_open_allocate_fresh_tokens() {
    let (mut pane, _, rep, _) = loaded_pane();
    pane.report = Some(rep.clone());
    let t0 = pane.token;
    pane.reload();
    assert_ne!(pane.token, t0);
    assert!(pane.report.is_none() && pane.previous.as_ref().is_some_and(|p| Arc::ptr_eq(p, &rep)) && pane.fresh);
    let t1 = pane.token;
    pane.open("other-v2");
    assert_ne!(pane.token, t1);
    assert!(pane.model.is_none() && pane.previous.is_none() && pane.model_id == "other-v2");
}

#[test]
fn resid_pipeline_one_request_per_phase() {
    let (worker, mut requests) = Worker::test_channel();
    let mut pane = ResidPane::new(String::new());
    // No id: list once.
    pane.drive(3, &worker);
    pane.drive(3, &worker);
    assert!(matches!(requests.try_recv(), Ok(Request::ListStudioFits { .. })));
    assert!(requests.try_recv().is_err());
    // An id: load once, then (with a model) score once, then check once.
    let (mut pane, _, rep, _) = loaded_pane();
    pane.model = None;
    pane.drive(3, &worker);
    pane.drive(3, &worker);
    assert!(matches!(requests.try_recv(), Ok(Request::LoadStudioFit { ref id, .. }) if id == "bench-v1"));
    assert!(requests.try_recv().is_err());
    let (mut pane, fit, _, _) = loaded_pane();
    pane.drive(3, &worker);
    pane.drive(3, &worker);
    assert!(matches!(requests.try_recv(), Ok(Request::ScoreStudioResidual { token: 41, .. })));
    assert!(requests.try_recv().is_err());
    pane.accept_scored(41, fit.content_digest, Ok(rep));
    pane.drive(3, &worker);
    pane.drive(3, &worker);
    assert!(matches!(requests.try_recv(), Ok(Request::CheckStudioResidual { token: 41, .. })));
    assert!(requests.try_recv().is_err());
}

#[test]
fn resid_sample_mode_clears_and_requests_nothing() {
    let (mut pane, fit, rep, chk) = loaded_pane();
    pane.report = Some(rep.clone());
    pane.previous = Some(rep);
    pane.checks = Some(chk);
    pane.accept_scored(41, fit.content_digest, Ok(pane.report.clone().unwrap()));
    pane.previous = pane.report.clone();
    pane.clear_for_sample();
    assert!(pane.report.is_none() && pane.previous.is_none() && pane.checks.is_none());
    let (worker, mut requests) = Worker::test_channel();
    frame(&egui::Context::default(), &mut pane, &worker, true);
    assert!(pane.report.is_none() && pane.previous.is_none() && pane.checks.is_none());
    assert!(pane.sample_text.as_deref().is_some_and(|t| t.contains("no provider calls")));
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
        panic!("RESID sample-mode test must never download daily bars");
    }
}

#[tokio::test]
async fn resid_score_refuses_in_sample_mode_with_zero_provider_calls() {
    let history = tr_data::history::Service::new(
        tr_data::history::Archive::memory().unwrap(),
        Arc::new(NoNetwork),
        Default::default(),
    );
    let bridge = tr_data::whats_news::Config { enabled: false, ..Default::default() }.bridge();
    let (_, fit, _, _) = loaded_pane();
    let calls = AtomicUsize::new(0);
    let err = score_research(&CorrCache::default(), false, None, &history, &bridge, true, &fit, |_, _| {
        calls.fetch_add(1, SeqCst);
        async { anyhow::bail!("must not be called") }
    })
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("sample"), "{err:#}");
    assert_eq!(calls.load(SeqCst), 0, "sample mode made provider calls");
}

#[test]
fn resid_layout_roundtrip_and_legacy_route() {
    let settings = PaneSettings::StudioResid { model_id: "AAPL-v-SPY-v1".into() };
    let json = serde_json::to_string(&settings).unwrap();
    assert!(!json.contains("residuals") && !json.contains("token"), "{json}");
    let back: PaneSettings = serde_json::from_str(&json).unwrap();
    assert_eq!(back, settings);
    let PaneKind::StudioResid(p) = PaneKind::from_settings(back) else { panic!("not a RESID pane") };
    assert_eq!(p.model_id, "AAPL-v-SPY-v1");
    // The pane saves only its id.
    assert_eq!(
        PaneKind::StudioResid(p).settings(),
        PaneSettings::StudioResid { model_id: "AAPL-v-SPY-v1".into() }
    );
    // Partial JSON defaults every field.
    let partial: PaneSettings = serde_json::from_str(r#"{"StudioResid":{}}"#).unwrap();
    let PaneKind::StudioResid(p) = PaneKind::from_settings(partial) else { panic!() };
    assert!(p.model_id.is_empty());
    // The legacy unavailable-route layout restores an empty RESID pane.
    let legacy: PaneSettings = serde_json::from_str(r#"{"StudioRoute":{"route":"Resid"}}"#).unwrap();
    let PaneKind::StudioResid(p) = PaneKind::from_settings(legacy) else { panic!("legacy route is not a RESID pane") };
    assert!(p.model_id.is_empty() && p.model.is_none());
    // ATTR keeps its unavailable route.
    let attr: PaneSettings = serde_json::from_str(r#"{"StudioRoute":{"route":"Attr"}}"#).unwrap();
    assert!(matches!(PaneKind::from_settings(attr), PaneKind::StudioRoute(_)));
    // A hand-edited path never becomes an id.
    let evil: PaneSettings = serde_json::from_str(r#"{"StudioResid":{"model_id":"../../etc/passwd"}}"#).unwrap();
    let PaneKind::StudioResid(p) = PaneKind::from_settings(evil) else { panic!() };
    assert!(p.model_id.is_empty());
}

#[test]
fn resid_panel_paints_without_requests_and_selection_is_free() {
    let (mut pane, fit, rep, chk) = loaded_pane();
    pane.accept_scored(41, fit.content_digest, Ok(rep));
    pane.accept_checked(41, fit.content_digest, pane.report.as_ref().unwrap().key, Ok(chk));
    let (worker, mut requests) = Worker::test_channel();
    let ctx = egui::Context::default();
    frame(&ctx, &mut pane, &worker, false);
    assert!(requests.try_recv().is_err(), "an accepted report must not request");
    pane.ui.show_z = true;
    frame(&ctx, &mut pane, &worker, false);
    pane.ui.show_z = false;
    frame(&ctx, &mut pane, &worker, false);
    assert!(requests.try_recv().is_err(), "chart selection dispatched work");
}
