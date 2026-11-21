use super::*;
use crate::{
    app::{Pane, tests::test_app},
    command::{self, Command, Function},
    layouts::PaneSettings,
    worker::{Request, Response},
};
use std::sync::Arc;
fn pane() -> StudioPane {
    StudioPane::new(StudioViewSpec {
        context: StudioContext {
            symbol: "AAPL US Equity".into(),
            tf: Timeframe::D1,
            selected_span: Some((20, 40)),
            as_of: None,
        },
        ..Default::default()
    })
}
fn app_pane(app: &mut TerminalApp, id: PaneId) -> &mut StudioPane {
    let PaneKind::Studio(s) = &mut app.panes.iter_mut().find(|p| p.id == id).unwrap().kind else { panic!("Studio") };
    s
}
#[test]
fn studio_legacy_commands_preserved() {
    for (cmd, kind) in [
        ("AAPL DIST 5m", "dist"),
        ("DIST H=5", "shape"),
        ("AAPL CORR QQQ 1h", "corr"),
        ("AAPL SEASON 30m", "season"),
        ("AAPL COND RSI(14) < 30", "when"),
        ("AAPL BACKTEST", "bt"),
    ] {
        let f = match command::parse(cmd).unwrap() {
            Command::Function(f) | Command::SymbolFunction(_, f) => f,
            _ => panic!("{cmd}"),
        };
        assert!(
            matches!(
                (kind, f),
                ("dist", Function::StudioDist { .. })
                    | ("shape", Function::StudioDistHorizon(5))
                    | ("corr", Function::StudioCorr { .. })
                    | ("season", Function::StudioSeason { .. })
                    | ("when", Function::Stats { conditional: true, .. })
                    | ("bt", Function::Backtest { .. })
            ),
            "{cmd}"
        );
    }
    let Some(Command::SymbolFunction(s, Function::Studio { spec })) = command::parse("AAPL STUDIO 5m lens=dist") else {
        panic!("shell command")
    };
    assert_eq!(s, "AAPL US Equity");
    assert_eq!(tr_data::studio::parse_spec(&spec).unwrap().1, Timeframe::M5);
    for lens in StudioLens::ALL {
        assert!(matches!(
            command::parse(&format!("STUDIO lens={}", lens.code())),
            Some(Command::Function(Function::Studio { .. }))
        ));
    }
    for r in [
        tr_data::studio::StudioRoute::Lab,
        tr_data::studio::StudioRoute::Resid,
        tr_data::studio::StudioRoute::Attr,
    ] {
        assert_eq!(command::parse(&format!("STUDIO {}", r.code())), Some(Command::Function(Function::StudioRoute(r))));
        // LAB is built (slice 5); RESID / ATTR still explain why they are unavailable.
        assert!(r.reason().contains(if r == tr_data::studio::StudioRoute::Lab { "Regression Lab" } else { "unavailable" }));
    }
    assert!(matches!(command::parse("AAPL QUICK QQQ 5m mode=index100"),Some(Command::SymbolFunction(_,Function::StudioQuick {..}))));
    assert!(command::parse("STUDIO lens=typo").is_none());
}
#[test]
fn studio_relate_parse_routes_h_to_relate() {
    use tr_data::studio::{BetaMode, MatrixMode, Transform};
    let (lens, _, benchmark, inputs) =
        parse_studio("lens=relate benchmark=QQQ transform=logreturn h=5 beta=expanding matrix=common bands=off").unwrap();
    assert_eq!(lens, StudioLens::Relate);
    assert_eq!(benchmark, "QQQ");
    assert_eq!(inputs.relate_transform, Some(Transform::LogReturn { h: 5 }));
    assert_eq!((inputs.relate_beta, inputs.relate_matrix, inputs.relate_bands), (BetaMode::Expanding, MatrixMode::Common, false));
    // Level transforms take no horizon; every h outside {1, 5, 20} is refused.
    assert_eq!(parse_studio("lens=relate transform=loglevel").unwrap().3.relate_transform, Some(Transform::LogLevel));
    assert!(parse_studio("lens=relate transform=logreturn h=7").is_err());
    assert!(parse_studio("lens=relate transform=nope").is_err());
    assert!(parse_studio("lens=dist transform=level").is_err(), "relate words require lens=relate");
    // h= still routes to When for lens=when, and stays an error elsewhere.
    let when = parse_studio("lens=when h=6").unwrap();
    assert_eq!((when.0, when.3.when_horizon), (StudioLens::When, 6));
    assert!(parse_studio("lens=dist h=5").is_err());
    assert!(matches!(
        command::parse("STUDIO lens=relate benchmark=QQQ transform=logreturn h=5"),
        Some(Command::Function(Function::Studio { .. }))
    ));
    assert!(command::parse("STUDIO lens=relate h=7").is_none());
    // RELATE words: bare opens a Relate Studio; invalid arguments fall through to the ticker.
    assert_eq!(command::parse("RELATE"), Some(Command::Function(Function::StudioRelate { spec: String::new() })));
    assert_eq!(
        command::parse("RELATE transform=level bands=off"),
        Some(Command::Function(Function::StudioRelate { spec: "transform=level bands=off".into() }))
    );
    assert_eq!(command::parse("RELATE US Equity"), Some(Command::Symbol("RELATE US Equity".into())));
    assert!(!matches!(command::parse("RELATE h=7"), Some(Command::Function(Function::StudioRelate { .. }))));
}
#[test]
fn relate_inputs_serde_old_layout_defaults() {
    use tr_data::studio::{BetaMode, MatrixMode};
    // An old saved view has none of the Relate fields: defaults apply (bands on, rolling, pairwise).
    let old: StudioViewSpec = serde_json::from_str(r#"{"lens":"Relate","inputs":{"corr_horizon":5}}"#).unwrap();
    assert_eq!(old.inputs.corr_horizon, 5);
    assert_eq!(old.inputs.relate_transform, None);
    assert_eq!((old.inputs.relate_beta, old.inputs.relate_matrix, old.inputs.relate_bands), (BetaMode::Rolling, MatrixMode::Pairwise, true));
    // The default Relate transform follows the legacy horizon.
    assert_eq!(old.inputs.relate_spec("QQQ US Equity").transform, tr_data::studio::Transform::LogReturn { h: 5 });
    let mut v = StudioViewSpec::default();
    v.inputs.relate_transform = Some(tr_data::studio::Transform::Change { h: 20 });
    v.inputs.relate_matrix = MatrixMode::Common;
    assert_eq!(serde_json::from_str::<StudioViewSpec>(&serde_json::to_string(&v).unwrap()).unwrap(), v);
}
#[test]
fn relate_command_updates_the_focused_studio_without_opening_a_pane() {
    use tr_data::studio::{BetaMode, Transform};
    let mut app = test_app();
    app.open_studio(Some("AAPL"), "lens=relate benchmark=QQQ");
    let before = app.panes.len();
    let focus = app.focus;
    app.open_relate(None, "transform=change h=20 beta=expanding");
    assert_eq!(app.panes.len(), before, "RELATE with arguments adjusts the focused Studio");
    let i = app.idx(focus).unwrap();
    let PaneKind::Studio(s) = &mut app.panes[i].kind else { panic!("studio") };
    assert_eq!(s.lens, StudioLens::Relate);
    let spec = s.corr_mut().unwrap().relate.clone().unwrap();
    assert_eq!((spec.transform, spec.beta_mode), (Transform::Change { h: 20 }, BetaMode::Expanding));
    assert_eq!(s.corr_mut().unwrap().horizon, 20);
    // Bare RELATE opens a Relate Studio (a status line names it).
    app.open_relate(None, "");
    assert!(app.status.starts_with("Relate"), "{}", app.status);
    assert!(app.panes.iter().any(|p| matches!(&p.kind, PaneKind::Studio(s) if s.lens == StudioLens::Relate)));
}
#[test]
fn when_commands_route_and_persist_backward_compatibly() {
    let Some(Command::SymbolFunction(sym, Function::StudioWhen { spec })) = command::parse("AAPL WHEN 5m RSI(14)<30")
    else {
        panic!("AAPL WHEN parses")
    };
    assert_eq!(sym, "AAPL US Equity");
    let cmd = tr_data::studio_when::parse_command(&spec).unwrap();
    assert_eq!((cmd.tf, cmd.rule.as_deref()), (Some(Timeframe::M5), Some("RSI(14)<30")));
    let s = when_studio(sym, cmd).view();
    assert_eq!((s.lens, s.context.tf, s.inputs.condition.as_str()), (StudioLens::When, Timeframe::M5, "RSI(14)<30"));
    let (lens, tf, _, inputs) = parse_studio("5m lens=when rule=\"RSI(14)<30 AND close>SMA(50)\" h=12").unwrap();
    assert_eq!((lens, tf, inputs.when_horizon), (StudioLens::When, Timeframe::M5, 12));
    assert_eq!(inputs.condition, "RSI(14)<30 AND close>SMA(50)");
    assert!(parse_studio("5m lens=dist rule=\"RSI(14)<30\"").is_err(), "rule needs lens=when");
    assert!(matches!(
        command::parse("AAPL STUDIO 5m lens=when rule=\"RSI(14)<30\""),
        Some(Command::SymbolFunction(_, Function::Studio { .. }))
    ));
    assert!(matches!(command::parse("WHEN h=6 threshold=10bp"), Some(Command::Function(Function::StudioWhen { .. }))));
    assert!(command::parse("WHEN h=5").is_none());
    // `WHEN h=…` adjusts the focused Studio instead of opening a new pane.
    let mut app = test_app();
    app.execute("AAPL WHEN 5m RSI(14)<30");
    let panes = app.panes.len();
    let id = app.panes.iter().find(|p| matches!(p.kind, PaneKind::Studio(_))).unwrap().id;
    app.focus = id;
    app.execute("WHEN h=12 threshold=15bp");
    assert_eq!(app.panes.len(), panes);
    let p = app_pane(&mut app, id).when_mut().unwrap();
    assert_eq!((p.horizon, p.threshold, p.draft_threshold), (12, 15., 15.));
    let copy = app_pane(&mut app, id).command();
    assert!(copy.contains("lens=when") && copy.contains("rule=\"RSI(14)<30\"") && copy.contains("h=12"), "{copy}");
    // Panel menu and older layouts.
    assert!(crate::app::workspace::PANELS.iter().any(|(_, w)| *w == "WHEN"));
    let Some(PaneKind::Studio(w)) = app.panel_from_spec("WHEN 15m close>SMA(20)", Some("MSFT")) else { panic!() };
    assert_eq!((w.lens, w.context.tf), (StudioLens::When, Timeframe::M15));
    let old: PaneSettings =
        serde_json::from_str(r#"{"Studio":{"view":{"lens":"When","inputs":{"condition":"RSI(14)<30"}}}}"#).unwrap();
    let PaneSettings::Studio { view } = old else { panic!() };
    assert_eq!((view.inputs.when_threshold, view.inputs.when_horizon), (10., 6));
}
#[test]
fn studio_layout_defaults_load() {
    let p: PaneSettings = serde_json::from_str(r#"{"Studio":{}}"#).unwrap();
    let PaneSettings::Studio { view } = p else { panic!() };
    assert_eq!(view, StudioViewSpec::default());
    let mut s = pane();
    s.context.selected_span = Some((20, 40));
    s.ensure_child();
    let settings = PaneKind::Studio(Box::new(s)).settings();
    let json = serde_json::to_string(&settings).unwrap();
    assert!(!json.contains("report") && !json.contains("generation") && !json.contains("children"));
    let round: PaneSettings = serde_json::from_str(&json).unwrap();
    assert_eq!(round, settings);
    // Restored Relate inputs are normalised: unsupported h → 1, corr_horizon ∈ {1,5,20}.
    let bad = StudioInputs {
        corr_horizon: 7,
        relate_transform: Some(tr_data::studio::Transform::Change { h: 3 }),
        ..Default::default()
    };
    assert_eq!(bad.valid_corr_horizon(), 1);
    assert_eq!(bad.relate_spec("SPY").transform, tr_data::studio::Transform::Change { h: 1 });
    let bad = StudioInputs { corr_horizon: 9, ..Default::default() };
    assert_eq!(bad.relate_spec("SPY").transform, tr_data::studio::Transform::LogReturn { h: 1 });
    let ok = StudioInputs { corr_horizon: 20, ..Default::default() };
    assert_eq!(ok.valid_corr_horizon(), 20);
    assert_eq!(ok.relate_spec("SPY").transform, tr_data::studio::Transform::LogReturn { h: 20 });
    for legacy in [r#"{"StudioDist":{}}"#, r#"{"StudioCorr":{}}"#, r#"{"StudioSeason":{}}"#] {
        assert!(serde_json::from_str::<PaneSettings>(legacy).is_ok());
    }
}
#[test]
fn studio_lens_roundtrip_preserves_selection() {
    let (worker, mut requests) = Worker::test_channel();
    let mut s = prepared(&worker, &mut requests, false);
    s.context.selected_span = Some((20, 40));
    let before = format!("{:?}", s.dist_mut().unwrap().report.as_ref().unwrap()).into_bytes();
    let ctx = egui::Context::default();
    // A committed selection asks once for the selected/rest stage.
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        s.analysis(ui, 201, &worker, false);
    });
    out.textures_delta.clear();
    let Ok(Request::StudioSubset { generation, input, selection, .. }) = requests.try_recv() else {
        panic!("selection requests the subset stage")
    };
    assert_eq!(selection, (20, 40));
    let key = input.bars_seed;
    s.accept_subset(generation, key, selection, super::studio_when::cached_subset(&input, selection));
    assert!(s.subset.is_some() || s.subset_error.is_some());
    for lens in [StudioLens::Relate, StudioLens::Season, StudioLens::When, StudioLens::Backtest, StudioLens::Dist] {
        s.select_lens(lens);
        s.ensure_child();
    }
    assert_eq!(s.context.selected_span, Some((20, 40)));
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        s.analysis(ui, 201, &worker, false);
    });
    out.textures_delta.clear();
    assert_eq!(format!("{:?}", s.dist_mut().unwrap().report.as_ref().unwrap()).into_bytes(), before);
    assert!(requests.try_recv().is_err(), "accepted roundtrip dispatched compute work");
    assert_eq!(s.children.len(), 5);
}
#[test]
fn studio_late_response_cannot_cross_lens_epoch() {
    let (worker, mut requests, tx) = Worker::test_channel_with_responses();
    let mut app = test_app();
    app.worker = worker;
    app.provider = "real".into();
    let mut s = pane();
    s.ensure_child();
    s.dist_mut().unwrap().request(301, &app.worker);
    let Request::StudioDist { generation: old, .. } = requests.try_recv().unwrap() else { panic!() };
    s.select_lens(StudioLens::Relate);
    s.ensure_child();
    s.select_lens(StudioLens::Dist);
    s.dist_mut().unwrap().request(301, &app.worker);
    let Request::StudioDist { generation: current, .. } = requests.try_recv().unwrap() else { panic!() };
    assert!(current > old);
    app.panes.push(Pane { id: 301, kind: PaneKind::Studio(Box::new(s)), link: None, title: None, window: None });
    tx.send(Response::StudioDist { pane: 301, generation: old, result: Err("obsolete error".into()) }).unwrap();
    app.drain();
    let d = app_pane(&mut app, 301).dist_mut().unwrap();
    assert!(d.loading && d.error.is_none());
    tx.send(Response::StudioDist { pane: 301, generation: current, result: Err("current unavailable".into()) })
        .unwrap();
    app.drain();
    assert_eq!(app_pane(&mut app, 301).dist_mut().unwrap().error.as_deref(), Some("current unavailable"));
    let epoch = app_pane(&mut app, 301).context_epoch;
    app_pane(&mut app, 301).set_context_symbol("MSFT US Equity");
    assert!(app_pane(&mut app, 301).context_epoch > epoch);
    assert!(app_pane(&mut app, 301).children.is_empty());
    tx.send(Response::StudioDist { pane: 301, generation: current, result: Err("context obsolete".into()) }).unwrap();
    app.drain();
    assert!(app_pane(&mut app, 301).children.is_empty());
}
#[test]
fn studio_undo_restores_inputs() {
    let mut s = pane();
    let original = s.view();
    s.remember(original.clone());
    s.context.tf = Timeframe::M5;
    s.benchmark = "QQQ".into();
    s.inputs.horizon = 5;
    s.invalidate();
    assert!(s.undo_inputs());
    assert_eq!(s.view(), original);
    for i in 0..25 {
        let before = s.view();
        s.context.selected_span = Some((i, i + 1));
        s.remember(before);
    }
    assert_eq!(s.undo.len(), 20);
    assert!(s.undo_inputs());
    assert_eq!(s.context.selected_span, Some((23, 24)));
}
#[test]
fn studio_sample_clears_all_children() {
    let (worker, mut requests) = Worker::test_channel();
    let mut s = prepared(&worker, &mut requests, false);
    for lens in StudioLens::ALL {
        s.select_lens(lens);
        s.ensure_child();
    }
    assert_eq!(s.children.len(), 5);
    assert!(s.dist_mut().unwrap().report.is_some());
    assert!(!s.chart.is_empty());
    s.sample_mode(true);
    assert!(s.children.is_empty() && s.chart.is_empty());
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        s.show(ui, 201, &worker, true, true);
    });
    out.textures_delta.clear();
    assert!(s.children.is_empty());
    assert!(requests.try_recv().is_err(), "sample mode dispatched provider/compute requests");
    s.sample_mode(false);
    assert!(s.children.is_empty());
    assert!(!s.chart_started);
}
#[test]
fn studio_embedded_chart_receives_history() {
    let (worker, mut requests, tx) = Worker::test_channel_with_responses();
    let mut app = test_app();
    app.worker = worker;
    app.provider = "real".into();
    let mut s = pane();
    s.chart.id = 302;
    s.chart.load(&app.worker);
    let generation = loop {
        match requests.try_recv().unwrap() {
            Request::History { generation, .. } => break generation,
            _ => {}
        }
    };
    app.panes.push(Pane { id: 302, kind: PaneKind::Studio(Box::new(s)), link: None, title: None, window: None });
    let bars = Arc::new(tr_perf::fixtures::daily(200, 78));
    let history = |generation| Response::History {
        pane: 302,
        generation,
        symbol: "AAPL US Equity".into(),
        tf: Timeframe::D1,
        bars: bars.clone(),
        gpu: tr_chart::scene::SeriesGpu::build(&bars),
        prepended: 0,
        from_cache: false,
        exhausted: true,
        live: false,
    };
    tx.send(history(generation - 1)).unwrap();
    app.drain();
    assert!(app.chart_mut(302).unwrap().is_empty());
    tx.send(history(generation)).unwrap();
    app.drain();
    assert!(!app.chart_mut(302).unwrap().is_empty());
    tx.send(Response::HistoryError { pane: 302, generation: generation - 1, error: "old history error".into() })
        .unwrap();
    app.drain();
    assert!(!app.chart_mut(302).unwrap().is_empty());
    // The native target path also operates on the embedded chart and invalidates dependent children.
    app.focus = 302;
    assert!(app.focused_is_chart());
    app.with_chart(|c, w| c.set_compare(Some("QQQ US Equity".into()), w));
    assert_eq!(app.chart_mut(302).unwrap().compare.as_deref(), Some("QQQ US Equity"));
    let line = tr_chart::scene::line_ref(&[1., 2.]);
    tx.send(Response::Compare { pane: 302, generation, symbol: "QQQ US Equity".into(), line }).unwrap();
    let mut batch = tr_data::news::NewsBatch::empty(1234);
    batch.queried = vec!["AAPL US Equity".into()];
    batch.items = vec![tr_data::NewsItem {
        title: "Verified test headline".into(),
        publisher: "test only".into(),
        link: "https://example.invalid/story".into(),
        ts: 1234,
        tickers: vec!["AAPL".into()],
    }];
    tx.send(Response::News { pane: 302, symbols: Some(vec!["AAPL US Equity".into()]), batch }).unwrap();
    tx.send(Response::HistoryError { pane: 302, generation, error: "current native error".into() }).unwrap();
    app.drain();
    let (_, n, compare, news, error) = app.chart_mut(302).unwrap().studio_test_receipt();
    assert_eq!(n, 200);
    assert!(compare);
    assert_eq!(news, 1);
    assert_eq!(error, Some("current native error"));
}
#[test]
fn studio_context_commands_invalidate_retained_reports() {
    let (worker, mut requests) = Worker::test_channel();
    let mut app = test_app();
    app.worker = worker;
    let s = prepared(&app.worker, &mut requests, false);
    app.panes.push(Pane { id: 201, kind: PaneKind::Studio(Box::new(s)), link: None, title: None, window: None });
    app.focus = 201;
    app.with_chart(|c, w| c.set_timeframe(Timeframe::M5, w));
    let s = app_pane(&mut app, 201);
    assert_eq!(s.context.tf, Timeframe::M5);
    assert!(s.children.is_empty());
}
#[test]
fn studio_local_bindings_registered_and_unique() {
    use crate::prefs::{Action as A, Keymap};
    use egui::{Key, Modifiers};
    let k = Keymap::default();
    for (key, m, a) in [
        (Key::ArrowLeft, Modifiers::ALT, A::StudioPrevLens),
        (Key::ArrowRight, Modifiers::ALT, A::StudioNextLens),
        (Key::Escape, Modifiers::NONE, A::StudioCloseInspector),
        (Key::Z, Modifiers { ctrl: true, command: !cfg!(target_os = "macos"), ..Modifiers::NONE }, A::StudioUndo),
    ] {
        assert_eq!(k.action_for(key, m), Some(a));
        let chord = k.chord(a).unwrap();
        assert_eq!(k.0.keys().filter(|b| k.chord(**b) == Some(chord)).count(), 1);
    }
    let mut app = test_app();
    app.panes.push(Pane { id: 303, kind: PaneKind::Studio(Box::new(pane())), link: None, title: None, window: None });
    app.focus = 303;
    app.run_hotkey(A::StudioNextLens);
    assert_eq!(app_pane(&mut app, 303).lens, StudioLens::Relate);
    app.run_hotkey(A::StudioPrevLens);
    assert_eq!(app_pane(&mut app, 303).lens, StudioLens::Dist);
    app_pane(&mut app, 303).inspector_open = true;
    app.run_hotkey(A::StudioCloseInspector);
    assert!(!app_pane(&mut app, 303).inspector_open);
}

#[test]
fn studio_cycles_supported_lenses_and_retains_explicit_unavailable_routes() {
    let mut s = pane();
    s.context.tf = Timeframe::H4;
    s.select_lens(StudioLens::Relate);
    s.cycle_lens(1);
    assert_eq!(s.lens, StudioLens::Dist);
    s.context.tf = Timeframe::M5;
    s.select_lens(StudioLens::Relate);
    s.cycle_lens(1);
    // Slice 3: When supports native 5m cash sessions.
    assert_eq!(s.lens, StudioLens::When);
    s.cycle_lens(1);
    assert_eq!(s.lens, StudioLens::Season);
    let (worker, mut requests) = Worker::test_channel();
    let ctx = egui::Context::default();
    for lens in [StudioLens::When, StudioLens::Season, StudioLens::Backtest] {
        s.context.tf = Timeframe::H4;
        s.select_lens(lens);
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            s.analysis(ui, 201, &worker, false);
        });
        out.textures_delta.clear();
    }
    assert!(requests.try_recv().is_err(), "unsupported intervals dispatched research");
}

#[test]
fn studio_text_editor_owns_parameter_undo() {
    use egui::{Key, Modifiers};
    let mut app = test_app();
    app.focus_cmd = false;
    app.cmd.clear();
    let mut s = pane();
    let before = s.view();
    s.context.tf = Timeframe::M5;
    s.remember(before);
    let old = s.view();
    app.panes.push(Pane { id: 304, kind: PaneKind::Studio(Box::new(s)), link: None, title: None, window: None });
    app.focus = 304;
    let ctx = egui::Context::default();
    let mut text = "editing".to_string();
    let edit_id = egui::Id::new("studio-test-editor");
    ctx.run_ui(egui::RawInput::default(), |ui| {
        ui.add(egui::TextEdit::singleline(&mut text).id(edit_id)).request_focus();
    })
    .textures_delta
    .clear();
    let mods = Modifiers { ctrl: true, command: !cfg!(target_os = "macos"), ..Modifiers::NONE };
    ctx.run_ui(
        egui::RawInput {
            events: vec![egui::Event::Key {
                key: Key::Z,
                physical_key: Some(Key::Z),
                pressed: true,
                repeat: false,
                modifiers: mods,
            }],
            ..Default::default()
        },
        |ui| {
            app.hotkeys(ui.ctx());
            ui.add(egui::TextEdit::singleline(&mut text).id(edit_id));
        },
    )
    .textures_delta
    .clear();
    assert_eq!(app_pane(&mut app, 304).view(), old);
    assert_eq!(app_pane(&mut app, 304).undo.len(), 1);
}

#[test]
fn studio_relate_roundtrip_reuses_accepted_arc_and_inputs() {
    let (worker, mut requests) = Worker::test_channel();
    let mut s = pane();
    s.benchmark = "QQQ".into();
    s.select_lens(StudioLens::Relate);
    s.ensure_child();
    let (a, b) = tr_perf::fixtures::relationship(300);
    let report = Arc::new(tr_data::studio_corr::compute("AAPL", "QQQ", &a, &b, 252., &[], "test only").unwrap());
    let bytes = format!("{report:?}").into_bytes();
    let c = s.corr_mut().unwrap();
    c.generation = studio_token();
    c.accept_shared(c.generation, Ok(report.clone()));
    s.select_lens(StudioLens::Season);
    s.select_lens(StudioLens::Relate);
    let ctx = egui::Context::default();
    ctx.run_ui(egui::RawInput::default(), |ui| {
        s.analysis(ui, 201, &worker, false);
    })
    .textures_delta
    .clear();
    let r = s.corr_mut().unwrap().report.as_ref().unwrap();
    assert!(Arc::ptr_eq(r, &report));
    assert_eq!(format!("{r:?}").into_bytes(), bytes);
    assert!(requests.try_recv().is_err());
}

#[test]
fn studio_history_change_and_queued_sample_responses_cannot_restore_reports() {
    let (worker, mut requests, tx) = Worker::test_channel_with_responses();
    let mut app = test_app();
    app.worker = worker;
    app.provider = "real".into();
    let mut s = prepared(&app.worker, &mut requests, false);
    let generation = s.dist_mut().unwrap().generation;
    let report = (**s.dist_mut().unwrap().report.as_ref().unwrap()).clone();
    app.panes.push(Pane { id: 201, kind: PaneKind::Studio(Box::new(s)), link: None, title: None, window: None });
    let epoch = app_pane(&mut app, 201).context_epoch;
    tx.send(Response::HistoryChanged).unwrap();
    tx.send(Response::StudioDist { pane: 201, generation, result: Ok(report.clone()) }).unwrap();
    app.drain();
    let s = app_pane(&mut app, 201);
    assert!(s.children.is_empty());
    assert!(s.context_epoch > epoch);
    s.ensure_child();
    let worker = &app.worker;
    let PaneKind::Studio(s) = &mut app.panes.iter_mut().find(|p|p.id==201).unwrap().kind else {panic!()};
    s.dist_mut().unwrap().request(201, worker);
    let generation = s.dist_mut().unwrap().generation;
    app.provider = "sample".into();
    tx.send(Response::StudioDist { pane: 201, generation, result: Ok(report) }).unwrap();
    app.drain();
    let s = app_pane(&mut app, 201);
    assert!(s.children.is_empty() && s.chart.is_empty());
}
