use super::*;
use crate::{
    app::{PaneKind, tests::test_app},
    layouts::PaneSettings,
    worker::{Request, Response, Worker},
};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use tr_data::whats_news::equity_engine::{EngineParams, engine_board};

const DAY: i64 = 86_400;
/// Monday 2025-01-06 00:00 UTC.
const MON: i64 = 1_736_121_600;

/// Weekday bars from `start` (a Monday); open = previous close so every bar is valid.
fn series(start: i64, closes: &[f64]) -> Vec<Bar> {
    let mut out = vec![];
    let mut prev = closes[0];
    for (i, c) in closes.iter().enumerate() {
        let ts = start + ((i / 5) as i64 * 7 + (i % 5) as i64) * DAY;
        out.push(Bar::new(ts, prev, prev.max(*c), prev.min(*c), *c, 1000.0).with_provenance(Provenance::Real));
        prev = *c;
    }
    out
}
/// 40 complete weeks then Mon–Wed of a partial week: closes jump to `jump` on the last Friday.
fn jumper(start: i64, base: f64, jump: f64) -> Vec<Bar> {
    let closes: Vec<f64> = (0..203).map(|i| if i < 199 { base } else { jump }).collect();
    series(start, &closes)
}
fn alternating(start: i64) -> Vec<Bar> {
    let closes: Vec<f64> =
        (0..203).map(|i| if i == 202 || i == 199 { 105.0 } else if i % 2 == 0 { 100.0 } else { 110.0 }).collect();
    series(start, &closes)
}
fn histories() -> Vec<(String, Vec<Bar>)> {
    // 15 weeks + Mon–Wed, ending on the same Wednesday as the 203-bar series.
    let nowk: Vec<f64> = (0..78).map(|i| if i < 77 { 100.0 } else { 110.0 }).collect();
    vec![
        ("BRK".into(), jumper(MON, 100.0, 110.0)),
        ("DWN".into(), jumper(MON, 100.0, 90.0)),
        ("MID".into(), alternating(MON)),
        ("NOWK".into(), series(MON + 175 * DAY, &nowk)),
        // Same shape, but its last bar is two weeks older than the others.
        ("OLD".into(), jumper(MON - 14 * DAY, 100.0, 110.0)),
    ]
}
fn snapshot_of(h: &[(String, Vec<Bar>)], provenance: Provenance) -> Arc<ViewSnapshot> {
    Arc::new(ViewSnapshot {
        board: Arc::new(engine_board(h, EngineParams::default())),
        skipped: vec![],
        source: "Local Yahoo daily research archive".into(),
        provenance,
    })
}
fn pane(scan: &str) -> ResearchPane {
    let mut p = ResearchPane::new(Some(scan.into()));
    p.accept_view(Ok(snapshot_of(&histories(), Provenance::Cached)));
    p
}
fn ts(i: usize) -> i64 {
    MON + ((i / 5) as i64 * 7 + (i % 5) as i64) * DAY
}
fn key(symbol: &str) -> String {
    tr_data::security(symbol).key()
}
fn select(p: &mut ResearchPane, symbol: &str) {
    p.selected = p.visible_symbols().iter().position(|s| *s == symbol).unwrap_or_else(|| panic!("{symbol} not visible"));
}

#[test]
fn fixture_rows_have_the_intended_reads() {
    let p = pane("VIEW PATTERN");
    let board = &p.view.as_ref().unwrap().snapshot.as_ref().unwrap().board;
    let row = |s: &str| board.rows.iter().find(|r| r.symbol == s).unwrap();
    let d = |s: &str| row(s).pattern_d.as_ref().unwrap().kind;
    let w = |s: &str| row(s).weekly.as_ref().unwrap().pattern.as_ref().ok().and_then(|p| p.kind);
    assert_eq!((d("BRK"), w("BRK")), (Some(PatternKind::Breakout), Some(PatternKind::Breakout)));
    assert_eq!((d("DWN"), w("DWN")), (Some(PatternKind::Breakdown), Some(PatternKind::Breakdown)));
    assert_eq!((d("MID"), w("MID")), (None, None));
    assert_eq!(d("NOWK"), Some(PatternKind::Breakout));
    assert!(row("NOWK").weekly.as_ref().unwrap().pattern.is_err());
    assert_eq!(row("BRK").pattern_d.as_ref().unwrap().trigger_ts, ts(202));
    assert_eq!(row("BRK").weekly.as_ref().unwrap().trigger_ts, ts(199), "last completed week's last session");
    assert_ne!(ts(202), ts(199));
}

#[test]
fn scan_view_layout_roundtrip_restores_view() {
    let settings = PaneSettings::Research(Some("VIEW RSI-C N=9 K=3".into()));
    let json = serde_json::to_string(&settings).unwrap();
    let back: PaneSettings = serde_json::from_str(&json).unwrap();
    let kind = PaneKind::from_settings(back);
    let PaneKind::Research(r) = &kind else { panic!("not a research pane") };
    let spec = r.view.as_ref().unwrap().spec.clone().unwrap();
    assert_eq!(spec.view, ScanView::RsiCounter);
    assert_eq!((spec.params.rsi_n, spec.params.lag), (9, 3));
    assert_eq!(kind.tab_title(), "Scan view: RSI Counter");
    assert_eq!(kind.settings(), PaneSettings::Research(Some("VIEW RSI-C N=9 K=3".into())));
    // Defaults are not written, and lower-case input persists canonically.
    let kind = PaneKind::from_settings(PaneSettings::Research(Some("view pattern".into())));
    assert_eq!(kind.settings(), PaneSettings::Research(Some("VIEW PATTERN".into())));
    assert_eq!(kind.tab_title(), "Scan view: Pattern");
    // Ordinary setups still behave.
    let kind = PaneKind::from_settings(PaneSettings::Research(Some("NEAR_HIGH".into())));
    let PaneKind::Research(r) = &kind else { panic!() };
    assert!(r.view.is_none());
    assert_eq!(kind.tab_title(), "Scan: NEAR_HIGH");
}

#[test]
fn scan_view_unknown_view_shows_error_sends_no_request() {
    let (worker, mut requests) = Worker::test_channel();
    let mut p = ResearchPane::new(Some("VIEW FOO".into()));
    assert!(p.error.as_deref().unwrap().contains("Unknown view FOO"));
    p.request(1, &worker);
    assert!(p.error.as_deref().unwrap().contains("Unknown view FOO"));
    assert!(!p.loading);
    assert!(requests.try_recv().is_err(), "a bad view must not reach the worker");
    // A valid view sends ScanView, never Research.
    let mut ok = ResearchPane::new(Some("VIEW PATTERN".into()));
    ok.request(2, &worker);
    assert!(matches!(requests.try_recv(), Ok(Request::ScanView { pane: 2, generation: 1, .. })));
    assert!(requests.try_recv().is_err());
}

#[test]
fn scan_view_bucket_click_filters_members() {
    let mut p = pane("VIEW PATTERN");
    let view = p.view.as_mut().unwrap();
    // Default: same-date cohort with hits only (MID has no pattern; OLD is older-dated).
    assert_eq!(view.symbols(), ["BRK", "DWN", "NOWK"]);
    assert_eq!(view.counts.get(Bucket::DPat(PatternKind::Breakout)), 2);
    assert_eq!(view.counts.get(Bucket::DPat(PatternKind::Breakdown)), 1);
    assert_eq!(view.counts.get(Bucket::WPat(PatternKind::Breakout)), 1);
    view.toggle_bucket(Bucket::DPat(PatternKind::Breakout));
    let mut s = view.symbols();
    s.sort();
    assert_eq!(s, ["BRK", "NOWK"]);
    assert_eq!(view.side, Side::D);
    view.toggle_bucket(Bucket::WPat(PatternKind::Breakdown));
    assert_eq!(view.symbols(), ["DWN"]);
    assert_eq!(view.side, Side::W, "a weekly bucket defaults to the weekly side");
    view.toggle_bucket(Bucket::WPat(PatternKind::Breakdown));
    assert_eq!(view.bucket, None);
    assert_eq!(view.symbols().len(), 3);
    view.hits_only = false;
    view.refilter("");
    assert_eq!(view.symbols().len(), 4, "All rows adds MID, still without the older-dated row");
    view.refilter("br");
    assert_eq!(view.symbols(), ["BRK"]);
}

#[test]
fn scan_view_enter_daily_sends_open_archive_at_trigger_ts() {
    let mut p = pane("VIEW PATTERN");
    select(&mut p, "BRK");
    assert_eq!(p.navigate(Key::Enter), Some(Action::OpenArchiveAt { symbol: key("BRK"), ts: ts(202) }));
}

#[test]
fn scan_view_right_arrow_enter_sends_weekly_trigger_ts() {
    let mut p = pane("VIEW PATTERN");
    select(&mut p, "BRK");
    assert_eq!(p.navigate(Key::ArrowRight), None);
    assert_eq!(p.view.as_ref().unwrap().side, Side::W);
    assert_eq!(p.navigate(Key::Enter), Some(Action::OpenArchiveAt { symbol: key("BRK"), ts: ts(199) }));
    p.navigate(Key::ArrowLeft);
    assert_eq!(p.navigate(Key::Enter), Some(Action::OpenArchiveAt { symbol: key("BRK"), ts: ts(202) }));
    // The weekly trigger is a daily bar, never the Monday bucket timestamp.
    assert_ne!(ts(199) % (7 * DAY), 0);
}

#[test]
fn scan_view_weekly_unavailable_no_daily_substitution() {
    let mut p = pane("VIEW PATTERN");
    select(&mut p, "NOWK");
    p.navigate(Key::ArrowRight);
    assert_eq!(p.navigate(Key::Enter), None, "no daily fallback for a weekly open");
    let notice = p.view.as_ref().unwrap().notice.clone().unwrap();
    assert!(notice.contains("weekly unavailable") && notice.contains("NOWK"), "{notice}");
    p.navigate(Key::ArrowLeft);
    assert!(matches!(p.navigate(Key::Enter), Some(Action::OpenArchiveAt { .. })));
    assert!(p.view.as_ref().unwrap().notice.is_none());
}

#[test]
fn scan_view_header_sort_resorts_without_worker_request() {
    let (worker, mut requests) = Worker::test_channel();
    let mut p = ResearchPane::new(Some("VIEW PATTERN".into()));
    p.request(1, &worker);
    assert!(requests.try_recv().is_ok());
    p.accept_view(Ok(snapshot_of(&histories(), Provenance::Cached)));
    let view = p.view.as_mut().unwrap();
    view.hits_only = false;
    view.refilter("");
    view.set_sort(Col::Ticker);
    assert_eq!(view.symbols(), ["BRK", "DWN", "MID", "NOWK"], "ticker sorts ascending first");
    view.set_sort(Col::Ticker);
    assert_eq!(view.symbols(), ["NOWK", "MID", "DWN", "BRK"]);
    view.set_sort(Col::DPos);
    view.set_sort(Col::DPos);
    assert!(requests.try_recv().is_err(), "sorting is local");
    // Positions: 1.0 (BRK), 0.5 (MID), 0.0 (DWN) and None (NOWK: flat prior window); None sorts last both ways.
    let view = p.view.as_mut().unwrap();
    view.sort = (Col::DPos, true);
    view.refilter("");
    assert_eq!(view.symbols(), ["BRK", "MID", "DWN", "NOWK"]);
    view.set_sort(Col::DPos);
    assert_eq!(view.symbols(), ["DWN", "MID", "BRK", "NOWK"]);
}

#[test]
fn scan_view_older_dated_rows_excluded_from_counts_by_default() {
    let mut p = pane("VIEW PATTERN");
    let view = p.view.as_mut().unwrap();
    assert_eq!(view.counts.older, 1);
    assert_eq!(view.counts.get(Bucket::DPat(PatternKind::Breakout)), 2);
    view.include_older = true;
    view.recount();
    view.refilter("");
    assert_eq!(view.counts.get(Bucket::DPat(PatternKind::Breakout)), 3, "OLD counts once included");
    assert!(view.symbols().contains(&"OLD"));
    view.include_older = false;
    view.recount();
    view.refilter("");
    assert!(!view.symbols().contains(&"OLD"));
    // Older rows keep their own date.
    let board = view.snapshot.as_ref().unwrap().board.clone();
    let old = board.rows.iter().find(|r| r.symbol == "OLD").unwrap();
    assert!(old.last_ts < board.as_of_ts.unwrap());
}

#[test]
fn scan_view_rejects_sample_and_unverified_snapshots() {
    for bad in [Provenance::Sample, Provenance::Missing] {
        let mut p = ResearchPane::new(Some("VIEW PATTERN".into()));
        p.accept_view(Ok(snapshot_of(&histories(), bad)));
        assert!(p.view.as_ref().unwrap().snapshot.is_none(), "{bad:?}");
        assert!(p.error.is_some());
    }
    // A refused refresh keeps the last verified board.
    let mut p = pane("VIEW RSI-C");
    p.accept_view(Ok(snapshot_of(&histories(), Provenance::Sample)));
    assert!(p.view.as_ref().unwrap().snapshot.is_some() && p.error.is_some());
    // The sample-mode refusal drops the board entirely: no digits painted.
    p.accept_view(Err(super::SAMPLE_REFUSAL.to_string()));
    let v = p.view.as_ref().unwrap();
    assert!(v.snapshot.is_none() && v.order.is_empty());
    assert!(p.error.is_some());
}

#[tokio::test]
async fn scan_view_sample_mode_refuses_with_zero_provider_calls() {
    let calls = AtomicUsize::new(0);
    let spec = ScanViewSpec::parse("VIEW RSI-C").unwrap().unwrap();
    let err = scan_view_research(
        true,
        || {
            calls.fetch_add(1, SeqCst);
            async { anyhow::bail!("must not be called") }
        },
        &spec,
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("sample"), "{err:#}");
    assert_eq!(calls.load(SeqCst), 0, "sample mode touched the loader");
    // Verified input computes; a Sample bar anywhere refuses the whole board.
    let ok = scan_view_research(false, || async { Ok((histories(), vec![], "src".into(), Provenance::Cached)) }, &spec)
        .await
        .unwrap();
    assert_eq!(ok.board.rows.len(), 5);
    let mut h = histories();
    h[2].1[7].provenance = Provenance::Sample;
    let err = scan_view_research(false, || async { Ok((h, vec![], "src".into(), Provenance::Cached)) }, &spec)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("sample"));
    let err = scan_view_research(false, || async { Ok((histories(), vec![], "src".into(), Provenance::Missing)) }, &spec)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").to_ascii_lowercase().contains("unverified") || format!("{err:#}").contains("refused"));
    // A second identical request is served from the content-keyed cache.
    let again = scan_view_research(false, || async { Ok((histories(), vec![], "src".into(), Provenance::Cached)) }, &spec)
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&ok.board, &again.board));
}

#[test]
fn scan_view_response_does_not_touch_scan_breadth() {
    let (worker, mut requests, tx) = Worker::test_channel_with_responses();
    let mut app = test_app();
    app.worker = worker;
    app.open_panel(PaneKind::Research(Box::new(ResearchPane::new(Some("VIEW PATTERN".into())))));
    let (id, generation) = {
        let pane = app.panes.iter().find(|p| matches!(&p.kind, PaneKind::Research(_))).unwrap();
        let PaneKind::Research(r) = &pane.kind else { unreachable!() };
        (pane.id, r.generation)
    };
    assert!(matches!(requests.try_recv(), Ok(Request::ScanView { .. })));
    assert!(app.holding_risk.scan_breadth.is_none());
    // A stale generation is dropped.
    tx.send(Response::ScanView { pane: id, generation: generation + 9, result: Ok(snapshot_of(&histories(), Provenance::Cached)) })
        .unwrap();
    app.drain();
    let PaneKind::Research(r) = &app.panes.iter().find(|p| p.id == id).unwrap().kind else { unreachable!() };
    assert!(r.view.as_ref().unwrap().snapshot.is_none());
    tx.send(Response::ScanView { pane: id, generation, result: Ok(snapshot_of(&histories(), Provenance::Cached)) }).unwrap();
    app.drain();
    let PaneKind::Research(r) = &app.panes.iter().find(|p| p.id == id).unwrap().kind else { unreachable!() };
    assert_eq!(r.view.as_ref().unwrap().symbols().len(), 3);
    assert!(app.holding_risk.scan_breadth.is_none() && app.holding_risk.scan_breadth_received.is_none());
}

#[test]
fn scan_view_paints_both_views_and_the_tooltip_text_is_exact() {
    let ctx = egui::Context::default();
    for (scan, needles) in [
        ("VIEW PATTERN", ["▲ BREAKOUT", "D PATTERN", "W WKS"]),
        ("VIEW RSI-C", ["D STATE", "RSI 2…21", "Δ14/5"]),
    ] {
        let mut p = pane(scan);
        let (worker, _rx) = Worker::test_channel();
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1500., 800.))),
                ..Default::default()
            },
            |ui| {
                p.show(ui, &worker);
            },
        );
        out.textures_delta.clear();
        let mut text = String::new();
        fn walk(s: &egui::Shape, out: &mut String) {
            match s {
                egui::Shape::Text(t) => {
                    out.push_str(&t.galley.job.text);
                    out.push('\n');
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        out.shapes.iter().for_each(|s| walk(&s.shape, &mut text));
        for n in needles {
            assert!(text.contains(n), "{scan}: missing {n} in {text}");
        }
        assert!(text.contains("D as of 2025-"), "{text}");
    }
    // Tooltip strings.
    let p = pane("VIEW PATTERN");
    let board = &p.view.as_ref().unwrap().snapshot.as_ref().unwrap().board;
    let brk = board.rows.iter().find(|r| r.symbol == "BRK").unwrap();
    let tip = pattern_tip(brk.pattern_d.as_ref().unwrap(), false);
    assert!(tip.starts_with("C 110.00 ≥ H63 110.00") && tip.contains("trigger Wed 2025-"), "{tip}");
    let wtip = pattern_tip(brk.weekly.as_ref().unwrap().pattern.as_ref().unwrap(), true);
    assert!(wtip.contains("0.999 × H40wk") && wtip.contains("(incl. this week)") && wtip.contains("trigger Fri"), "{wtip}");
}

#[test]
fn scan_view_rsic_buckets_and_strip_tip() {
    let mut p = pane("VIEW RSI-C");
    let view = p.view.as_mut().unwrap();
    // Every cohort row with a daily counter lands in exactly one daily family bucket.
    let total: usize = FAMS.iter().map(|f| view.counts.get(Bucket::D(*f))).sum();
    assert_eq!(total, view.counts.cohort);
    let board = view.snapshot.as_ref().unwrap().board.clone();
    let row = board.rows.iter().find(|r| r.symbol == "BRK").unwrap();
    let x: f32 = RSIC_COLS.iter().take_while(|c| c.col != Col::Strip).map(|c| c.w).sum::<f32>() + 4.0 + 5.0 * STRIP_CELL + 2.0;
    let tip = tip_at(ScanView::RsiCounter, row, x).unwrap();
    assert!(tip.starts_with("RSI(7) = ") && tip.ends_with("reference EWM"), "{tip}");
}
