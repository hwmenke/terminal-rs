//! Headless egui CPU paint/tessellation gate. No adapter, display, network or GPU.
use super::*;
use crate::perf::{self, Layer};
use std::collections::BTreeMap;
use tr_perf::{Budget, Summary, Verdict};

fn chart(w: &Worker, auction: bool, n: usize) -> ChartState {
    let now = chrono::Utc::now().timestamp();
    let bars = Arc::new(if auction {
        tr_perf::fixtures::intraday(Timeframe::M1, 20, 5)
    } else {
        tr_perf::fixtures::crypto_m5(n, Timeframe::M5.bucket_start(now))
    });
    let mut c = ChartState::new(
        201,
        if auction { "AAPL US Equity" } else { "BTCUSD Curncy" },
        if auction { Timeframe::M1 } else { Timeframe::M5 },
    );
    c.indicators.clear();
    c.on_history(c.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, w);
    c.last_refresh = Some(if auction { bars.last().unwrap().ts + 60 } else { now });
    c.show_volume = true;
    c.show_rail = true;
    c.avwap_anchors = [0, 1, 2, 3].map(|i| bars[i * bars.len() / 5].ts).to_vec();
    let key = c.avwap_key(now).unwrap();
    let r = tr_data::anchored_vwap::report(&bars, &key.anchors, key.closed_len).unwrap();
    assert!(r.lines.iter().all(|l| l.as_ref().is_ok_and(|l| l.used > 0)));
    c.on_anchored_vwap(key, Ok(Arc::new(r)));
    if auction {
        c.on_vault_stitch(c.generation, Arc::new(tr_perf::fixtures::stitch(&bars, c.tf)));
        c.set_auction(crate::command::AuctionCmd::Toggle(Some(true)), w);
    } else {
        c.indicators = vec![IndicatorSpec::new("ZBAND", &[]).unwrap()];
        c.built = crate::worker::build_chart_indicators(
            &bars,
            c.tf,
            &c.indicators,
            &theme::palette(),
            c.sec.class,
            c.last_refresh,
            None,
        );
        assert!(c.built.iter().all(|b| b.refused.is_none() && b.honest_z.is_some()));
        c.set_trend_stack(Some(true), w);
        let lanes = ladder(c.tf, false)
            .into_iter()
            .skip(1)
            .map(|tf| {
                let mut b = tr_perf::fixtures::crypto_m5(400, 0);
                let last = tf.bucket_start(now);
                for (i, b) in b.iter_mut().enumerate() {
                    b.ts = last - (399 - i) as i64 * tf.seconds();
                }
                (tf, Ok(b))
            })
            .collect();
        let stack = tr_core::indicators::trend_stack(&bars, c.tf, lanes, Default::default()).unwrap();
        assert!(stack.lanes.iter().all(|l| matches!(l.status, LaneStatus::Ready)));
        c.on_trend_stack(c.generation, bars.len(), Ok(stack));
        c.show_edge = true;
        super::tests::complete_edge(&mut c, now);
        assert!(c.edge.as_ref().is_some_and(|r| r.as_ref().is_ok_and(|r| !r.completed.is_empty())));
        c.set_stop_cloud(crate::command::StopCmd::Toggle(Some(true)), w);
        c.set_trend_cone(Some(true), None, w);
        c.stack_pulse = None;
        c.stack_arrived = None;
        c.previous_stack_lanes = None;
    }
    c.viewport = Some(Viewport { right: bars.len() as f64 + 40., bar_px: 5., width_px: 1500. });
    c.request_session_summary(w);
    c
}
fn complete(c: &mut ChartState, rx: &mut tokio::sync::mpsc::UnboundedReceiver<Request>) {
    while let Ok(req) = rx.try_recv() {
        match req {
            Request::ChartSessionSummary { key, bars, class, stitch, .. } => {
                let r = tr_data::chart_session::build(&bars, key.tf, class, key.extended, key.as_of, stitch.as_deref())
                    .unwrap();
                assert!(r.stats.vwap.is_some() && r.opening.is_some() && r.prior.is_some());
                assert_eq!(r.coverage.len(), 20);
                c.on_session_summary(key, Ok(Arc::new(r)));
            }
            Request::StopCloud { key, .. } => {
                let r = super::tests::stop_result(c, &key).unwrap();
                assert!(r.closed > 0);
                c.on_stop_cloud(key, Ok(r));
            }
            Request::TrendCone { key, .. } => {
                let r = super::tests::cone_result(c, &key).unwrap();
                assert!(r.conditional.paths > 0);
                c.on_trend_cone(key, Ok(r));
            }
            Request::AuctionMap { key, .. } => {
                let r = tr_data::auction::build(
                    &c.bars,
                    c.tf,
                    None,
                    c.sec.class,
                    c.last_refresh.unwrap(),
                    &[],
                    key.params,
                    0.01,
                )
                .unwrap();
                assert_eq!(r.sessions.len(), 20);
                c.on_auction_map(key, Ok(Arc::new(r)));
            }
            _ => {}
        }
    }
}
fn frame(ctx: &egui::Context, c: &mut ChartState, w: &Worker, index: usize) -> egui::FullOutput {
    let mut out = ctx.run_ui(
        egui::RawInput {
            time: Some(ctx.input(|i| i.time) + 0.2),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600., 900.))),
            events: vec![egui::Event::PointerMoved(pos2(600. + (index % 2) as f32 * 10., 350.))],
            ..Default::default()
        },
        |ui| {
            c.show(ui, Rect::from_min_size(pos2(10., 10.), vec2(1580., 880.)), true, w);
        },
    );
    out.textures_delta.clear();
    out
}
fn text(shapes: &[egui::epaint::ClippedShape]) -> String {
    fn walk(s: &egui::Shape) -> String {
        match s {
            egui::Shape::Text(t) => t.galley.job.text.clone(),
            egui::Shape::Vec(v) => v.iter().map(walk).collect::<Vec<_>>().join("\n"),
            _ => String::new(),
        }
    }
    shapes.iter().map(|s| walk(&s.shape)).collect::<Vec<_>>().join("\n")
}
fn assert_painted(c: &ChartState, out: &egui::FullOutput, auction: bool, sums: &[f32; 23]) {
    let txt = text(&out.shapes);
    let expected = if auction {
        vec!["AUCTION · bin", "AVWAP", "VWAP", "RVOL", "OR30", "PDH", "CONCENTRATION", "[0,1]", "CLOSED THRU", "VAULT"]
    } else {
        vec!["STOP ODDS", "P90", "P10", "TSTACK EDGE", "AVWAP", "ZBAND"]
    };
    for word in expected {
        assert!(txt.contains(word), "expected {word}; painted:\n{txt}");
    }
    for l in if auction {
        vec![Layer::Auction, Layer::Avwap]
    } else {
        vec![Layer::Stack, Layer::Stops, Layer::Cone, Layer::Edge, Layer::Avwap, Layer::ZBand, Layer::Rail]
    } {
        assert!(sums[l as usize] > 0., "no timing for {l:?}");
    }
    if auction {
        assert!(
            c.session_snapshot()
                .is_some_and(|s| s.coverage.len() == 20 && !s.stats.profile.is_empty() && s.stats.profile.len() <= 24)
        );
        assert!(c.concentration_paint_is_present());
    }
    if !auction {
        assert!(!c.stack_paint.cloud.is_empty());
        assert!(!c.stack_paint.lanes.is_empty());
        assert!(!c.zband_paint.cloud.is_empty());
        assert!(!c.stops.paint.shapes.is_empty());
        assert!(!c.cone.paint.shapes.is_empty());
    }
}
fn sample(auction: bool, n: usize) -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    let (w, mut rx) = Worker::test_channel();
    let mut c = chart(&w, auction, n);
    complete(&mut c, &mut rx);
    perf::set_enabled(true);
    for i in 0..20 {
        let out = frame(&ctx, &mut c, &w, i);
        ctx.tessellate(out.shapes, out.pixels_per_point);
        perf::end_frame();
    }
    let mut results = BTreeMap::new();
    let refresh_report = c.session.snapshot.clone();
    for mode in if auction { vec!["steady", "pan", "refresh"] } else { vec!["steady", "pan"] } {
        let pan = mode == "pan";
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            if pan {
                c.viewport.as_mut().unwrap().pan_px(if i % 2 == 0 { 24. } else { -24. });
            }
            let start = Instant::now();
            if mode == "refresh" {
                // Include actual response acceptance/formatting in timed UI work.
                super::session_cache::tests::deliver(&mut c, refresh_report.as_ref().unwrap().clone());
            }
            let out = frame(&ctx, &mut c, &w, i);
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            let sums = perf::end_frame();
            if i == 0 {
                assert_painted(&c, &out, auction, &sums);
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(out.shapes, out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (id, v) in [("ui".to_owned(), ui_us), ("tess".into(), tess_us), ("total".into(), ui_us + tess_us)] {
                times.entry(id).or_default().push(v);
            }
            for l in Layer::ALL.iter().skip(5) {
                times.entry(format!("layer.{l:?}")).or_default().push(sums[*l as usize] as f64 * 1000.);
            }
        }
        let scenario = if auction { "auction" } else { "intraday" };
        while let Ok(req) = rx.try_recv() {
            assert!(!matches!(req, Request::ChartSessionSummary { .. }), "unchanged frames sent a new summary job");
        }
        for (id, mut samples) in times {
            results.insert(
                if mode == "refresh" {
                    format!("paint.auction_refresh.{id}")
                } else {
                    format!("paint.{scenario}.{mode}.{id}")
                },
                tr_perf::summarize(&mut samples),
            );
        }
    }
    perf::set_enabled(false);
    results
}
fn paint_budget(id: &str, budgets: &tr_perf::Budgets) -> Budget {
    let us = if let Some((prefix, name)) = id.rsplit_once(".layer.") {
        let l = Layer::ALL.into_iter().find(|l| format!("{l:?}") == name).expect("layer id");
        l.budget_ms() as f64 * 1000. * if prefix.ends_with(".pan") { 4. } else { 1. }
    } else {
        budgets.cases.get(id).unwrap_or_else(|| panic!("missing paint budget {id}")).budget_us
    };
    Budget { budget_us: us, relative: false, class: "frame".into(), over_target: false }
}
#[test]
#[ignore = "perf gate; release only"]
fn perf_gate_paint() {
    assert!(
        !cfg!(debug_assertions) || std::env::var("TR_PERF_ALLOW_DEBUG").as_deref() == Ok("1"),
        "perf gate requires --release (TR_PERF_ALLOW_DEBUG=1 for diagnostics only)"
    );
    let slack = std::env::var("TR_PERF_SLACK").map(|v| v.parse::<f64>().expect("TR_PERF_SLACK number")).unwrap_or(1.);
    assert!(slack.is_finite() && slack > 0.);
    eprintln!("PAINT absolute gate; slack {slack}{}", if slack != 1. { " OVERRIDE" } else { "" });
    let budgets: tr_perf::Budgets = tr_perf::read_json(&tr_perf::root().join("perf/paint-budgets.json")).unwrap();
    assert_eq!(budgets.schema, 1, "paint budget schema");
    assert!(budgets.cases.values().all(|b| b.budget_us.is_finite() && b.budget_us > 0.0), "invalid paint ceiling");
    let mut all = BTreeMap::new();
    for auction in [false, true] {
        let mut best = sample(auction, 5000);
        for _ in 0..2 {
            if best.iter().all(|(id, s)| {
                !matches!(tr_perf::check(s, &paint_budget(id, &budgets), None, 1., slack), Verdict::Fail(_))
            }) {
                break;
            }
            for (id, s) in sample(auction, 5000) {
                if s.median_us < best[&id].median_us {
                    best.insert(id, s);
                }
            }
        }
        all.extend(best);
    }
    let metric_ids =
        all.keys().filter(|id| !id.contains(".layer.")).cloned().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        metric_ids,
        budgets.cases.keys().filter(|id| !id.starts_with("paint.studio_")).cloned().collect(),
        "stale/missing paint budgets"
    );
    let mut failures = vec![];
    for (id, s) in &all {
        let budget = paint_budget(id, &budgets);
        let verdict = tr_perf::check(s, &budget, None, 1., slack);
        eprintln!(
            "{id:48} {:9.2} us p95 {:9.2} / {:9.2} {:?}",
            s.median_us,
            s.p95_us,
            budget.budget_us * slack,
            verdict
        );
        if let Verdict::Fail(reason) = verdict {
            failures.push(format!("{id}: {reason}"));
        }
    }
    let mut record = tr_perf::Baseline::new(1.);
    record.cases = all;
    let paint_path = tr_perf::output_dir().join("paint.json");
    if let Ok(previous) = tr_perf::read_json::<tr_perf::Baseline>(&paint_path) {
        record.cases.extend(previous.cases.into_iter().filter(|(id, _)| id.starts_with("paint.studio_")));
    }
    tr_perf::write_json(&paint_path, &record).unwrap();
    assert!(failures.is_empty(), "paint gate failures:\n{}", failures.join("\n"));
    if std::env::var("TR_PERF_SAVE").as_deref() == Ok("1") {
        let path = tr_perf::root().join("perf/baseline.json");
        let mut base = tr_perf::Baseline::load(&path).unwrap().expect("save compute baseline first");
        base.cases.extend(record.cases);
        tr_perf::write_json(&path, &base).unwrap();
    }
}
#[test]
fn chart_show_records_layers_only_when_enabled() {
    let ctx = egui::Context::default();
    let (w, mut rx) = Worker::test_channel();
    let mut c = chart(&w, false, 500);
    complete(&mut c, &mut rx);
    for on in [false, true] {
        perf::set_enabled(on);
        let out = frame(&ctx, &mut c, &w, 0);
        let sums = perf::end_frame();
        if on {
            assert_painted(&c, &out, false, &sums);
        } else {
            assert_eq!(sums, [0.; 23]);
        }
    }
    perf::set_enabled(false);
}
#[test]
fn perf_scopes_do_not_change_painted_shapes() {
    fn render(on: bool) -> (usize, String) {
        let ctx = egui::Context::default();
        let (w, mut rx) = Worker::test_channel();
        let mut c = chart(&w, false, 500);
        complete(&mut c, &mut rx);
        perf::set_enabled(on);
        for i in 0..20 {
            frame(&ctx, &mut c, &w, i);
        }
        let out = frame(&ctx, &mut c, &w, 0);
        perf::set_enabled(false);
        (out.shapes.len(), text(&out.shapes))
    }
    assert_eq!(render(false), render(true));
}

/// Native canvas fixture shared with the integrated Studio gate; all preparation is untimed.
pub(crate) fn studio_canvas(
    w: &Worker,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Request>,
    integrated: bool,
) -> (ChartState, Arc<Vec<Bar>>) {
    if integrated {
        let mut c = chart(w, false, 5000);
        complete(&mut c, rx);
        assert!(c.gpu.as_ref().is_some_and(|g| g.len() == 5000));
        let bars = c.bars.clone();
        (c, bars)
    } else {
        let bars = Arc::new(tr_perf::fixtures::daily(5000, 71));
        let mut c = ChartState::new(201, "AAPL US Equity", Timeframe::D1);
        c.studio_reset();
        c.indicators.clear();
        c.show_news = false;
        c.on_history(c.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, w);
        c.last_refresh = Some(bars.last().unwrap().ts + 86400);
        assert_eq!(c.gpu.as_ref().unwrap().len(), 5000);
        (c, bars)
    }
}

pub(crate) fn studio_assert_canvas(c: &ChartState, out: &egui::FullOutput, integrated: bool) {
    assert!(c.gpu.as_ref().is_some_and(|g| g.len() == 5000));
    assert!(out.shapes.iter().any(|s| matches!(s.shape, egui::Shape::Callback(_))));
    if integrated {
        for word in ["AVWAP", "ZBAND"] {
            assert!(text(&out.shapes).contains(word), "integrated canvas missing {word}");
        }
        assert!(!c.stack_paint.cloud.is_empty() && !c.stack_paint.lanes.is_empty());
        assert!(!c.zband_paint.cloud.is_empty() && !c.stops.paint.shapes.is_empty() && !c.cone.paint.shapes.is_empty());
    }
}
