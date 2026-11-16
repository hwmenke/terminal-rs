//! Offline studio paint gate; fabricated fixtures exist only in dev dependencies.
use super::studio_season::{
    self,
    or_lens::{self, Selection},
};
use egui::{Pos2, Rect, pos2, vec2};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};
use tr_perf::{Baseline, Budgets, Summary, Verdict};

fn pane() -> studio_season::SeasonPane {
    let bars = tr_perf::fixtures::or_lens(120);
    let observed = bars.last().unwrap().ts + 1800;
    let r = tr_data::studio_season::compute_at(
        "AAPL US Equity",
        tr_core::Timeframe::M30,
        &bars,
        "benchmark fixture only",
        observed,
        observed + 86400,
    )
    .unwrap();
    let mut pane = studio_season::SeasonPane::new("AAPL", "30m");
    pane.generation = 1;
    pane.accept(1, Ok(r));
    let lens = pane.report.as_ref().unwrap().or_lens.as_ref().unwrap();
    assert_eq!(lens.opening_sessions, 120);
    for g in 0..3 {
        let c = lens.cell(g, 1, 0);
        assert!(c.selected.len() >= 30 && c.complement.len() >= 60);
        assert!(c.inference.interval.is_some());
    }
    pane
}
fn frame(
    ctx: &egui::Context,
    pane: &mut studio_season::SeasonPane,
    width: f32,
    hover: bool,
    index: usize,
) -> egui::FullOutput {
    let pointer = if hover {
        ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("or-probe"))).unwrap_or(Rect::NOTHING).center()
            + vec2(if index >= 160 { (index % 2) as f32 } else { 0. }, 0.)
    } else {
        pos2(15., 15.)
    };
    // A stationary physical pointer emits no PointerMoved event. Even a
    // duplicate event resets egui's tooltip delay.
    let events = if ctx.input(|i| i.pointer.hover_pos()) == Some(pointer) {
        vec![]
    } else {
        vec![egui::Event::PointerMoved(pointer)]
    };
    let mut out = ctx.run_ui(
        egui::RawInput {
            time: Some(1. + index as f64 * 0.2),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1200.))),
            events,
            ..Default::default()
        },
        |ui| {
            let cell =
                or_lens::view(ui, pane.report.as_ref().unwrap().or_lens.as_ref().unwrap(), &mut pane.or_selection);
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("or-probe"), cell));
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
fn guard(out: &egui::FullOutput) {
    let txt = text(&out.shapes);
    for word in [
        "OPENING RANGE",
        "Above",
        "Below",
        "Inside",
        "Δ bp",
        "CLOSED THRU",
        "NORMALIZED DISTRIBUTIONS",
        "Complement",
        "pointwise 95% HAC",
        "m=117",
    ] {
        assert!(txt.contains(word), "missing {word}: {txt}");
    }
    assert!(out.shapes.len() > 100, "empty geometry");
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
    let mut p = pane();
    for i in 0..20 {
        let out = frame(&ctx, &mut p, 1440., false, i);
        finite(&ctx, out);
    }
    let mut result = BTreeMap::new();
    for hover in [false, true] {
        let mode = if hover { "hover" } else { "steady" };
        if hover {
            // Respect egui's default still-pointer delay before measuring an
            // already-open card under small pointer motion.
            for i in 140..160 {
                let out = frame(&ctx, &mut p, 1440., true, i);
                finite(&ctx, out);
            }
        }
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            let start = Instant::now();
            let mut out = frame(&ctx, &mut p, 1440., hover, i + if hover { 160 } else { 20 });
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            if i == 60 {
                guard(&out);
                if hover {
                    assert!(text(&out.shapes).contains("OR MEAN CONTRAST"), "hover did not paint the MathCard");
                }
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
                times.entry(format!("paint.studio_or.{mode}.{metric}")).or_default().push(v);
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
fn perf_gate_paint_studios() {
    assert!(!cfg!(debug_assertions) || std::env::var("TR_PERF_ALLOW_DEBUG").as_deref() == Ok("1"), "release required");
    let slack = std::env::var("TR_PERF_SLACK").map(|v| v.parse::<f64>().unwrap()).unwrap_or(1.);
    assert!(slack.is_finite() && slack > 0.);
    eprintln!("STUDIO PAINT absolute gate; slack {slack}{}", if slack != 1. { " OVERRIDE" } else { "" });
    let budgets: Budgets = tr_perf::read_json(&tr_perf::root().join("perf/paint-budgets.json")).unwrap();
    assert_eq!(budgets.schema, 1);
    let mut best = sample();
    let ids: BTreeSet<_> = budgets.cases.keys().filter(|id| id.starts_with("paint.studio_or.")).cloned().collect();
    assert_eq!(best.keys().cloned().collect::<BTreeSet<_>>(), ids, "stale/missing studio budgets");
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
        record.cases.extend(previous.cases.into_iter().filter(|(id, _)| !id.starts_with("paint.studio_or.")));
    }
    record.cases.extend(best.clone());
    tr_perf::write_json(&path, &record).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    if std::env::var("TR_PERF_SAVE").as_deref() == Ok("1") {
        let path = tr_perf::root().join("perf/baseline.json");
        let mut base = Baseline::load(&path).unwrap().expect("save compute first");
        base.cases.extend(best);
        tr_perf::write_json(&path, &base).unwrap();
    }
}
#[test]
fn or_lens_selection_preserves_entire_q_family_and_finite_geometry() {
    let mut pane = pane();
    let qs = pane.report.as_ref().unwrap().or_lens.as_ref().unwrap().cells.iter().map(|c| c.q).collect::<Vec<_>>();
    for width in [320., 1440.] {
        let ctx = egui::Context::default();
        for (i, (g, h)) in [(0, 0), (1, 1), (2, 2)].into_iter().enumerate() {
            pane.or_selection = Selection { state: g, clock: 1, horizon: h };
            let out = frame(&ctx, &mut pane, width, false, i);
            if width > 1000. {
                guard(&out);
            } else {
                assert!(text(&out.shapes).contains("Δ bp"));
            }
            finite(&ctx, out);
        }
    }
    let ctx = egui::Context::default();
    for i in 140..166 {
        let out = frame(&ctx, &mut pane, 1440., i > 140, i);
        if i == 165 {
            assert!(text(&out.shapes).contains("OR MEAN CONTRAST"), "hover card missing");
        }
        finite(&ctx, out);
    }
    assert_eq!(
        qs,
        pane.report.as_ref().unwrap().or_lens.as_ref().unwrap().cells.iter().map(|c| c.q).collect::<Vec<_>>()
    );
}
#[test]
fn or_lens_cards_print_method_counts_and_unavailable_reason() {
    let pane = pane();
    let r = pane.report.as_ref().unwrap().or_lens.as_ref().unwrap();
    let card = or_lens::effect_card(r, r.cell(0, 1, 0));
    assert!(card.formula.contains("complement"));
    assert!(card.inputs.iter().any(|(k, v)| k.contains("bandwidth") && v.contains("L=min(5")));
    assert!(card.notes.iter().any(|s| s.contains("m=117")));
    assert!(card.source.is_some() && card.closed_thru.is_some() && card.span.is_some() && card.n.is_some());
    let missing = or_lens::effect_card(r, r.cell(0, 12, 0));
    assert!(missing.unavailable.is_some());
    let low = or_lens::effect_card(r, r.cell(2, 0, 0));
    assert!(low.unavailable.is_some() && low.notes.iter().any(|s| s.contains("LOW N")));
    assert!(or_lens::histogram_card(r, r.cell(0, 12, 0), 0).unavailable.is_some());
    assert!(or_lens::histogram_card(r, r.cell(0, 0, 0), 0).value.unwrap().starts_with("— /"));
}

#[test]
fn or_lens_sparse_flat_and_empty_complements_paint_finitely() {
    let mut bars = tr_perf::fixtures::or_lens(2);
    for b in &mut bars {
        b.open = 100.;
        b.close = 100.;
        b.high = 100.;
        b.low = 100.;
    }
    let observed = bars.last().unwrap().ts + 1800;
    let report =
        tr_data::studio_season::compute_at("AAPL", tr_core::Timeframe::M30, &bars, "test", observed, observed + 86400)
            .unwrap();
    let mut pane = studio_season::SeasonPane::new("AAPL", "30m");
    pane.generation = 1;
    pane.accept(1, Ok(report));
    for width in [320., 1440.] {
        let ctx = egui::Context::default();
        for (i, (state, clock)) in [(0, 1), (2, 1), (0, 12)].into_iter().enumerate() {
            pane.or_selection = Selection { state, clock, horizon: 0 };
            let out = frame(&ctx, &mut pane, width, false, i);
            assert!(text(&out.shapes).contains("Δ bp"));
            finite(&ctx, out);
        }
    }
}
