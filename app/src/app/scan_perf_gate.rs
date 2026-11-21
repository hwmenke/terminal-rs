//! Offline paint gate for SCAN VIEW RSI-C (500 rows). Fabricated fixtures live in dev dependencies only.
use super::research::{
    ResearchPane,
    scan_view::ViewSnapshot,
};
use crate::worker::Worker;
use egui::{Pos2, Rect, pos2, vec2};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};
use tr_data::whats_news::equity_engine::{EngineParams, engine_board};
use tr_perf::{Baseline, Budgets, Summary, Verdict};

const ROWS: usize = 500;

fn pane() -> ResearchPane {
    let histories: Vec<_> =
        (0..ROWS).map(|i| (format!("S{i:03}"), tr_perf::fixtures::daily(650, 9000 + i as u64))).collect();
    let board = engine_board(&histories, EngineParams::default());
    assert_eq!(board.rows.len(), ROWS);
    let mut p = ResearchPane::new(Some("VIEW RSI-C".into()));
    p.accept_view(Ok(Arc::new(ViewSnapshot {
        board: Arc::new(board),
        skipped: vec![],
        source: "benchmark fixture only".into(),
        provenance: tr_core::Provenance::Cached,
    })));
    assert_eq!(p.view.as_ref().unwrap().symbols().len(), ROWS, "all fixture rows share one as-of date");
    p
}
fn frame(ctx: &egui::Context, pane: &mut ResearchPane, worker: &Worker, hover: bool, index: usize) -> egui::FullOutput {
    let pointer = if hover {
        ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("scan-view-probe"))).unwrap_or(Rect::NOTHING).center()
            + vec2(if index >= 160 { (index % 2) as f32 } else { 0. }, 0.)
    } else {
        pos2(15., 15.)
    };
    // A stationary physical pointer emits no PointerMoved event; a duplicate would reset the tooltip delay.
    let events = if ctx.input(|i| i.pointer.hover_pos()) == Some(pointer) {
        vec![]
    } else {
        vec![egui::Event::PointerMoved(pointer)]
    };
    let mut out = ctx.run_ui(
        egui::RawInput {
            time: Some(1. + index as f64 * 0.2),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440., 1200.))),
            events,
            ..Default::default()
        },
        |ui| {
            pane.show(ui, worker);
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
fn rects(shapes: &[egui::epaint::ClippedShape]) -> usize {
    fn walk(s: &egui::Shape) -> usize {
        match s {
            egui::Shape::Rect(_) => 1,
            egui::Shape::Vec(v) => v.iter().map(walk).sum(),
            _ => 0,
        }
    }
    shapes.iter().map(|s| walk(&s.shape)).sum()
}
/// Meaningful work: a full screen of rows with the heat strip, badges and the header.
fn guard(out: &egui::FullOutput) {
    let txt = text(&out.shapes);
    for word in ["D STATE", "RSI 2…21", "Δ14/5", "D as of", "500 rows"] {
        assert!(txt.contains(word), "missing {word}: {}", &txt[..txt.len().min(400)]);
    }
    let tickers = txt
        .lines()
        .filter(|l| l.len() == 4 && l.starts_with('S') && l[1..].chars().all(|c| c.is_ascii_digit()))
        .count();
    assert!(tickers >= 30, "only {tickers} ticker cells painted");
    assert!(rects(&out.shapes) > 40 * 20, "fewer than ~40 painted rows of 20 strip cells: {}", rects(&out.shapes));
}
fn finite(ctx: &egui::Context, out: egui::FullOutput) {
    let primitives = ctx.tessellate(out.shapes, out.pixels_per_point);
    assert!(!primitives.is_empty());
    for p in primitives {
        assert!(p.clip_rect.is_finite());
        if let egui::epaint::Primitive::Mesh(m) = p.primitive {
            assert!(m.vertices.iter().all(|v| v.pos.is_finite() && v.uv.is_finite()));
        }
    }
}
fn sample() -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    let (worker, _requests) = Worker::test_channel();
    let mut p = pane();
    for i in 0..20 {
        let out = frame(&ctx, &mut p, &worker, false, i);
        finite(&ctx, out);
    }
    let mut result = BTreeMap::new();
    for hover in [false, true] {
        let mode = if hover { "hover" } else { "steady" };
        if hover {
            // Let egui's still-pointer tooltip delay elapse before measuring the open tooltip.
            for i in 140..160 {
                let out = frame(&ctx, &mut p, &worker, true, i);
                finite(&ctx, out);
            }
        }
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            let start = Instant::now();
            let mut out = frame(&ctx, &mut p, &worker, hover, i + if hover { 160 } else { 20 });
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            if i == 60 {
                guard(&out);
                if hover {
                    assert!(text(&out.shapes).contains("RSI(7) ="), "hover did not paint the heat-strip tooltip");
                }
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
                times.entry(format!("paint.scan_view.{mode}.{metric}")).or_default().push(v);
            }
        }
        for (id, mut samples) in times {
            result.insert(id, tr_perf::summarize(&mut samples));
        }
    }
    result
}
#[test]
#[ignore = "perf gate; release only"]
fn perf_gate_paint_scan_view() {
    assert!(!cfg!(debug_assertions) || std::env::var("TR_PERF_ALLOW_DEBUG").as_deref() == Ok("1"), "release required");
    let slack = std::env::var("TR_PERF_SLACK").map(|v| v.parse::<f64>().unwrap()).unwrap_or(1.);
    assert!(slack.is_finite() && slack > 0.);
    eprintln!("SCAN VIEW PAINT absolute gate; slack {slack}{}", if slack != 1. { " OVERRIDE" } else { "" });
    let budgets: Budgets = tr_perf::read_json(&tr_perf::root().join("perf/paint-budgets.json")).unwrap();
    assert_eq!(budgets.schema, 1);
    let mut best = sample();
    let ids: BTreeSet<_> = budgets.cases.keys().filter(|id| id.starts_with("paint.scan_")).cloned().collect();
    assert_eq!(best.keys().cloned().collect::<BTreeSet<_>>(), ids, "stale/missing scan-view budgets");
    for _ in 0..2 {
        if best
            .iter()
            .all(|(id, s)| !matches!(tr_perf::check(s, &budgets.cases[id], None, 1., slack), Verdict::Fail(_)))
        {
            break;
        }
        for (id, s) in sample() {
            if s.median_us < best[&id].median_us {
                best.insert(id, s);
            }
        }
    }
    let mut failures = vec![];
    for (id, s) in &best {
        let budget = &budgets.cases[id];
        let verdict = tr_perf::check(s, budget, None, 1., slack);
        eprintln!("{id:48} {:9.2} us p95 {:9.2} / {:9.2} {verdict:?}", s.median_us, s.p95_us, budget.budget_us * slack);
        if let Verdict::Fail(reason) = verdict {
            failures.push(format!("{id}: {reason}"));
        }
    }
    let path = tr_perf::output_dir().join("paint.json");
    let mut record = Baseline::new(1.);
    if let Ok(previous) = tr_perf::read_json::<Baseline>(&path) {
        record.cases.extend(previous.cases.into_iter().filter(|(id, _)| !id.starts_with("paint.scan_")));
    }
    record.cases.extend(best);
    tr_perf::write_json(&path, &record).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
