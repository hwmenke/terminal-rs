//! Offline UI/worker harness: real egui events and chart rendering, deterministic
//! responses, no window, GPU device, network, user settings or credentials.
use super::*;
use crate::palette::Target;
use crate::worker::Response;
use egui::{Event, Id, Key, Modifiers};
use std::sync::{Arc, mpsc};
use tr_chart::scene::SeriesGpu;
use tr_core::{AssetClass, Bar, Provenance, Security, Timeframe};

type Requests = tokio::sync::mpsc::UnboundedReceiver<Request>;

fn key(key: Key) -> Event {
    Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
}

fn frame(ctx: &egui::Context, app: &mut TerminalApp, events: Vec<Event>) {
    app.drain();
    ctx.run_ui(
        egui::RawInput {
            time: Some(ctx.input(|i| i.time) + 0.1),
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1100.0, 760.0))),
            events,
            ..Default::default()
        },
        |ui| {
            app.hotkeys(ui.ctx());
            app.command_palette(ui.ctx());
            app.command_field(ui, Rect::from_min_size(pos2(20.0, 30.0), vec2(700.0, 24.0)));
            for p in &mut app.panes {
                if let PaneKind::Chart(c) = &mut p.kind {
                    c.show(ui, Rect::from_min_size(pos2(20.0, 180.0), vec2(1040.0, 540.0)), true, &app.worker);
                }
            }
        },
    )
    .textures_delta
    .clear();
}

fn fixture() -> (TerminalApp, Requests, mpsc::Sender<Response>) {
    let mut app = tests::test_app();
    let (worker, requests, responses) = Worker::test_channel_with_responses();
    app.worker = worker;
    let id = app.new_pane(PaneKind::Chart(Box::new(ChartState::new(0, "AAPL", Timeframe::D1))));
    app.focus = id;
    app.dock = Node::leaf(id);
    app.focus_cmd = false;
    (app, requests, responses)
}

fn bars(symbol: &str, n: usize) -> Arc<Vec<Bar>> {
    let sec = Security::parse(symbol).unwrap();
    let close = match sec.class {
        AssetClass::Fx => 5.03,
        AssetClass::Govt => 4.123,
        AssetClass::Crypto => 65000.0,
        _ => 200.0,
    };
    Arc::new(
        (0..n)
            .map(|i| {
                Bar::new(1_790_000_000 + i as i64 * 86400, close, close + 0.01, close - 0.01, close, 1.0)
                    .with_provenance(Provenance::Sample)
            })
            .collect(),
    )
}

/// Deliver actual requests through the application's response drain. Both empty
/// successes and provider errors are exercised; indicators run the production builder.
fn respond(app: &mut TerminalApp, requests: &mut Requests, out: &mpsc::Sender<Response>, n: usize) {
    for _ in 0..8 {
        while let Ok(req) = requests.try_recv() {
            match req {
                Request::History { pane, generation, symbol, tf, .. } => {
                    assert!(Security::parse(&symbol).is_some(), "invalid worker symbol: {symbol}");
                    let bars = bars(&symbol, n);
                    out.send(Response::History {
                        pane,
                        generation,
                        symbol,
                        tf,
                        gpu: SeriesGpu::build(&bars),
                        bars,
                        prepended: 0,
                        from_cache: false,
                        exhausted: true,
                        live: false,
                    })
                    .unwrap();
                }
                Request::Indicators { pane, generation, bars, tf, specs, palette } => {
                    out.send(Response::Indicators {
                        pane,
                        generation,
                        built: crate::worker::build_indicators(&bars, tf, &specs, &palette),
                    })
                    .unwrap();
                }
                _ => {}
            }
        }
        app.drain();
    }
}

#[test]
fn headless_ticker_switch_paths() {
    let long = "A".repeat(100_000);
    let inputs = [
        ("SPY", Some("SPY US Equity")),
        ("AAPL US Equity", Some("AAPL US Equity")),
        ("SPX Index", Some("SPX Index")),
        ("BTC", Some("BTCUSD Curncy")),
        ("BTCUSD Curncy", Some("BTCUSD Curncy")),
        ("USDBRL CURNCY", Some("USDBRL Curncy")),
        ("BRL=X", Some("USDBRL Curncy")),
        ("USDBRL=X", Some("USDBRL Curncy")),
        ("BRL Curncy", Some("USDBRL Curncy")),
        ("usdbrl curncy", Some("USDBRL Curncy")),
        ("EURBRL Curncy", Some("EURBRL Curncy")),
        ("EURUSD Curncy", Some("EURUSD Curncy")),
        ("US10Y Govt", Some("US10Y Govt")),
        ("CL1 Comdty", Some("CL1 Comdty")),
        ("???", None),
        ("", None),
        (long.as_str(), None),
        ("ação", None),
    ];
    for path in ["command Enter", "palette pick", "chart typing", "recent symbol", "layout persistence"] {
        let ctx = egui::Context::default();
        let (mut app, mut requests, out) = fixture();
        respond(&mut app, &mut requests, &out, 300);
        frame(&ctx, &mut app, vec![]);
        for (step, (input, expected)) in inputs.iter().enumerate() {
            let before = app.focused_symbol().unwrap();
            let expected = expected.unwrap_or(&before);
            match path {
                "command Enter" => {
                    app.cmd = input.to_string();
                    app.command_changed();
                    app.focus_cmd = true;
                    frame(&ctx, &mut app, vec![]);
                    frame(&ctx, &mut app, vec![key(Key::Enter)]);
                }
                "chart typing" => {
                    ctx.memory_mut(|m| m.surrender_focus(Id::new("command-line")));
                    app.focus_cmd = false;
                    frame(&ctx, &mut app, vec![Event::Text(input.to_string())]);
                    frame(&ctx, &mut app, vec![key(Key::Enter)]);
                }
                "palette pick" => {
                    if let Some(expected) = inputs[step].1 {
                        app.palette.open();
                        app.palette.query = input.to_string();
                        frame(&ctx, &mut app, vec![]);
                        let items = app.palette_items();
                        let results = app.palette.results(&items);
                        app.palette.selected =
                            results.iter().position(|i| i.target == Target::Run(expected.into())).unwrap();
                        frame(&ctx, &mut app, vec![key(Key::Enter)]);
                        assert!(!app.palette.open);
                    } else {
                        app.palette.open();
                        app.palette.query = input.to_string();
                        frame(&ctx, &mut app, vec![]);
                        assert!(command::parse(input).is_none());
                        frame(&ctx, &mut app, vec![key(Key::Escape)]);
                    }
                }
                "recent symbol" => {
                    if let Some(expected) = inputs[step].1 {
                        app.recent_symbols.insert(0, expected.into());
                        app.cmd = expected.into();
                        app.command_changed();
                        app.focus_cmd = true;
                        frame(&ctx, &mut app, vec![]);
                        app.suggestion = app.suggestions().iter().position(|s| s.key == expected);
                        assert!(app.suggestion.is_some());
                        frame(&ctx, &mut app, vec![key(Key::Enter)]);
                    } else {
                        app.accept_command(Some(input));
                    }
                }
                "layout persistence" => {
                    app.open_security(input);
                    let old_id = app.focus;
                    let doc = app.current_doc();
                    app.store.save(&doc).unwrap();
                    app.apply_doc(app.store.load(&doc.name).unwrap());
                    assert!(app.focus > old_id, "pane IDs must not accept old layout responses");
                }
                _ => unreachable!(),
            }
            assert_eq!(app.focused_symbol().as_deref(), Some(expected), "{path}, input {input:?}");
            // Render the reset/loading state before any bars have arrived.
            frame(&ctx, &mut app, vec![]);
            respond(&mut app, &mut requests, &out, [0, 1, 2, 300][step % 4]);
            frame(&ctx, &mut app, vec![]);
            frame(&ctx, &mut app, vec![Event::PointerMoved(pos2(500.0, 400.0))]);
        }
    }
}

#[test]
fn stale_success_error_indicators_and_timeframe_responses_cannot_change_new_chart() {
    let ctx = egui::Context::default();
    let (mut app, mut requests, out) = fixture();
    let Request::History { pane, generation: old_generation, symbol: old_symbol, tf: old_tf, .. } =
        requests.try_recv().unwrap()
    else {
        panic!("history")
    };
    app.open_security("USDBRL CURNCY");
    let status = app.status.clone();
    let old_bars = bars(&old_symbol, 300);
    out.send(Response::History {
        pane,
        generation: old_generation,
        symbol: old_symbol,
        tf: old_tf,
        gpu: SeriesGpu::build(&old_bars),
        bars: old_bars,
        prepended: 0,
        from_cache: false,
        exhausted: false,
        live: false,
    })
    .unwrap();
    out.send(Response::HistoryError { pane, generation: old_generation, error: "stale failure".into() }).unwrap();
    out.send(Response::Indicators {
        pane,
        generation: old_generation,
        built: crate::worker::build_indicators(
            &bars("AAPL", 300),
            Timeframe::D1,
            &crate::command::default_indicators(),
            &theme::palette(),
        ),
    })
    .unwrap();
    app.drain();
    assert_eq!(app.status, status, "stale errors must not overwrite current status");
    assert!(app.chart_mut(pane).unwrap().is_empty());
    respond(&mut app, &mut requests, &out, 1);
    assert!(!app.chart_mut(pane).unwrap().is_empty());
    // Same symbol, older timeframe and generation, including A -> B -> A.
    app.open_security("AAPL");
    app.open_security("USDBRL CURNCY");
    app.with_chart(|c, w| c.set_timeframe(Timeframe::M5, w));
    out.send(Response::HistoryError { pane, generation: old_generation, error: "old timeframe".into() }).unwrap();
    app.drain();
    assert!(app.chart_mut(pane).unwrap().is_empty());
    respond(&mut app, &mut requests, &out, 2);
    frame(&ctx, &mut app, vec![]);
    assert_eq!(app.focused_symbol().as_deref(), Some("USDBRL Curncy"));
    assert_eq!(app.chart_mut(pane).unwrap().tf, Timeframe::M5);
}
