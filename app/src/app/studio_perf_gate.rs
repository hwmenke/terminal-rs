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
    best.extend(season_sample());
    best.extend(corr_sample());
    best.extend(regime_sample());
    best.extend(shape_sample());
    best.extend(shell_sample());
    best.extend(quick_sample());
    best.extend(when_sample());
    let ids: BTreeSet<_> = budgets.cases.keys().filter(|id| id.starts_with("paint.studio_")).cloned().collect();
    assert_eq!(best.keys().cloned().collect::<BTreeSet<_>>(), ids, "stale/missing studio budgets");
    for _ in 0..2 {
        if best
            .iter()
            .all(|(id, s)| !matches!(tr_perf::check(s, &budgets.cases[id], None, 1., slack), Verdict::Fail(_)))
        {
            break;
        }
        for (id, s) in sample()
            .into_iter()
            .chain(season_sample())
            .chain(corr_sample())
            .chain(regime_sample())
            .chain(shape_sample())
            .chain(shell_sample())
            .chain(quick_sample())
            .chain(when_sample())
        {
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
        record.cases.extend(previous.cases.into_iter().filter(|(id, _)| !id.starts_with("paint.studio_")));
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

fn season_pane() -> studio_season::SeasonPane {
    let bars = tr_perf::fixtures::season_evidence(120);
    let observed = bars.last().unwrap().ts + 300;
    let r = tr_data::studio_season::compute("AAPL", tr_core::Timeframe::M5, &bars, "benchmark fixture only", observed)
        .unwrap();
    let mut pane = studio_season::SeasonPane::new("AAPL", "5m");
    pane.generation = 1;
    pane.accept(1, Ok(r));
    let e = pane.report.as_ref().unwrap().evidence.as_ref().unwrap();
    assert_eq!((e.sessions, e.grid_sessions, e.expected_bars, e.family_size()), (120, 120, 9360, 468));
    assert!(e.cells[..78].iter().all(|c| c.inference.interval.is_some()));
    assert!(e.cells.iter().any(|c| c.inference.p.is_some() && c.q <= 0.10));
    pane.band = Some(std::sync::Arc::new(e.band_input.compute()));
    assert!(pane.band.as_ref().unwrap().critical.is_some());
    pane
}
fn season_frame(
    ctx: &egui::Context,
    pane: &mut studio_season::SeasonPane,
    width: f32,
    hover: bool,
    index: usize,
) -> egui::FullOutput {
    let probe_id = egui::Id::new("season-evidence-probe");
    let pointer = if hover {
        ctx.data(|d| d.get_temp::<Rect>(probe_id)).unwrap_or(Rect::NOTHING).center()
            + vec2(if index >= 160 { (index % 2) as f32 } else { 0. }, 0.)
    } else {
        pos2(15., 15.)
    };
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
            let rc = studio_season::evidence::view_with_band(
                ui,
                pane.report.as_ref().unwrap(),
                &mut pane.evidence_mode,
                pane.band.as_deref(),
            );
            ctx.data_mut(|d| d.insert_temp(probe_id, rc));
        },
    );
    out.textures_delta.clear();
    out
}
fn season_guard(out: &egui::FullOutput) {
    let txt = text(&out.shapes);
    for word in [
        "SEASON EVIDENCE",
        "ALL CLOCK",
        "ALL",
        "Mon",
        "Fri",
        "Mean bp",
        "CLOSED THRU",
        "Coverage: 9360/9360",
        "ALL family m=78",
        "Pointwise 95%",
        "LOW N",
        "ALL simultaneous 95% band",
    ] {
        assert!(txt.contains(word), "missing {word}: {txt}");
    }
    // Visible means, sample sizes and all-clock uncertainty must be actual work.
    assert!(out.shapes.len() > 400, "empty season geometry: {}", out.shapes.len());
    let vertical_whiskers=out.shapes.iter().filter(|s|matches!(&s.shape,egui::Shape::LineSegment {points,..} if points[0].x==points[1].x && (points[0].y-points[1].y).abs()>1.)).count();
    assert!(vertical_whiskers >= 78, "ALL curve whiskers missing: {vertical_whiskers}");
}
fn season_sample() -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    let mut p = season_pane();
    for i in 0..20 {
        let out = season_frame(&ctx, &mut p, 1440., false, i);
        finite(&ctx, out);
    }
    let mut result = BTreeMap::new();
    for hover in [false, true] {
        let mode = if hover { "hover" } else { "steady" };
        if hover {
            for i in 140..160 {
                let out = season_frame(&ctx, &mut p, 1440., true, i);
                finite(&ctx, out);
            }
        }
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            let start = Instant::now();
            let mut out = season_frame(&ctx, &mut p, 1440., hover, i + if hover { 160 } else { 20 });
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            if i == 60 {
                season_guard(&out);
                if hover {
                    assert!(text(&out.shapes).contains("SEASON CLOCK MEAN"), "hover did not paint the MathCard");
                }
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
                times.entry(format!("paint.studio_season.{mode}.{metric}")).or_default().push(v);
            }
        }
        for (id, mut samples) in times {
            result.insert(id, tr_perf::summarize(&mut samples));
        }
    }
    result
}
#[test]
fn season_modes_preserve_statistics_keys_and_finite_geometry() {
    let mut p = season_pane();
    let original = p.report.clone().unwrap();
    for width in [320., 1440.] {
        let ctx = egui::Context::default();
        for (i, mode) in [false, true].into_iter().enumerate() {
            p.evidence_mode = mode;
            let out = season_frame(&ctx, &mut p, width, false, i);
            let txt = text(&out.shapes);
            assert!(txt.contains(if mode { "EVIDENCE · filled only" } else { "DESCRIPTIVE · all observed" }));
            let e = original.evidence.as_ref().unwrap();
            let hs = studio_season::evidence::scale(e);
            if width > 1000. {
                season_guard(&out);
                for (_, label) in hs.entries() {
                    assert!(txt.contains(&label), "key missing {label}");
                }
            }
            let fills: Vec<_> = out
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Rect(rc) if rc.rect.height() == 8. => Some(rc.fill),
                    _ => None,
                })
                .collect();
            assert!(fills.len() > 30);
            let allowed: Vec<_> =
                hs.entries().iter().map(|(v, _)| hs.fill(*v)).chain([crate::theme::control()]).collect();
            assert!(fills.iter().all(|f| allowed.contains(f)));
            finite(&ctx, out);
            assert_eq!(p.report.as_ref().unwrap(), &original);
        }
    }
    let e = original.evidence.as_ref().unwrap();
    for c in &e.cells {
        assert_eq!(studio_season::evidence::filled(c, true), c.inference.p.is_some() && c.q <= 0.10);
        assert_eq!(studio_season::evidence::filled(c, false), c.inference.estimate.is_some());
    }
    let ctx = egui::Context::default();
    for i in 140..166 {
        let out = season_frame(&ctx, &mut p, 1440., i > 140, i);
        if i == 165 {
            assert!(text(&out.shapes).contains("SEASON CLOCK MEAN"));
        }
        finite(&ctx, out);
    }
}
#[test]
fn season_cards_expose_grid_uncertainty_and_fixed_family() {
    let p = season_pane();
    let r = p.report.as_ref().unwrap();
    let c = studio_season::evidence::card(r, 0, 1);
    for word in [
        "u[s]",
        "Var=Σu²",
        "L=min(5,T−1)=5",
        "p / BY q",
        "m=78 (ALL family",
        "CLOSED THRU",
        "pointwise",
        "not simultaneous",
    ] {
        assert!(c.to_text().contains(word), "missing {word}: {}", c.to_text());
    }
    assert_eq!(c.n.unwrap().0, 120);
    assert!(c.source.is_some() && c.span.is_some() && c.closed_thru.is_some());
    let low = studio_season::evidence::card(r, 1, 1);
    assert!(low.to_text().contains("LOW N") && low.to_text().contains("p/CI unavailable"));
}
#[test]
fn season_sparse_zero_and_empty_cells_paint_finitely() {
    let mut bars = tr_perf::fixtures::season_evidence(2);
    for b in &mut bars {
        b.close = b.open;
    }
    // Keep just the first two clock buckets; no invented afternoon values.
    bars.retain(|b| {
        let wall = tr_core::ny::civil(b.ts);
        chrono::Timelike::hour(&wall) == 9 && chrono::Timelike::minute(&wall) < 40
    });
    let now = bars.last().unwrap().ts + 300;
    let r = tr_data::studio_season::compute("AAPL", tr_core::Timeframe::M5, &bars, "test", now).unwrap();
    let missing = studio_season::evidence::card(&r, 0, 5);
    assert!(missing.unavailable.is_some() && missing.to_text().contains("n=0"));
    let mut p = studio_season::SeasonPane::new("AAPL", "5m");
    p.accept(0, Ok(r));
    for width in [320., 1440.] {
        for (i, mode) in [false, true].into_iter().enumerate() {
            p.evidence_mode = mode;
            let ctx = egui::Context::default();
            let out = season_frame(&ctx, &mut p, width, false, i);
            finite(&ctx, out);
        }
    }
}

fn corr_report() -> tr_data::studio_corr::CorrReport {
    let (a, b) = tr_perf::fixtures::relationship(5000);
    let r = tr_data::studio_corr::compute("AAPL", "QQQ", &a, &b, 252., &[], "benchmark fixture only").unwrap();
    assert_eq!(r.ols.n, 5000);
    assert!(r.robust.fits[2].intervals.iter().all(Option::is_some));
    assert_eq!(r.robust.rolling[2].len(), 1000);
    assert!(r.robust.holdout.as_ref().unwrap().r_squared_oos.is_some());
    r
}
fn corr_frame(
    ctx: &egui::Context,
    r: &tr_data::studio_corr::CorrReport,
    width: f32,
    hover: bool,
    index: usize,
) -> egui::FullOutput {
    let pointer = if hover {
        ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("corr-probe"))).unwrap_or(Rect::NOTHING).center()
            + vec2(if index >= 160 { (index % 2) as f32 } else { 0. }, 0.)
    } else {
        pos2(15., 15.)
    };
    let events = if ctx.input(|i| i.pointer.hover_pos()) == Some(pointer) {
        vec![]
    } else {
        vec![egui::Event::PointerMoved(pointer)]
    };
    let mut out = ctx.run_ui(
        egui::RawInput {
            time: Some(1. + index as f64 * 0.2),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1600.))),
            events,
            ..Default::default()
        },
        |ui| super::studio_corr::report_view(ui, 17, r),
    );
    out.textures_delta.clear();
    out
}
fn corr_guard(out: &egui::FullOutput) {
    let txt = text(&out.shapes);
    for word in [
        "HAC Bartlett L=5",
        "mean-response CI",
        "95% CI",
        "HOLDOUT",
        "R²_oos",
        "TRAIN",
        "TEST",
        "RETURN CLOUD",
        "ROLLING BETA",
        "CLOSED THRU",
    ] {
        assert!(txt.contains(word), "missing {word}: {txt}");
    }
    assert!(out.shapes.len() > 50, "empty relationship paint");
    let vertices: usize = out
        .shapes
        .iter()
        .map(|s| match &s.shape {
            egui::Shape::Mesh(m) => m.vertices.len(),
            _ => 0,
        })
        .sum();
    assert!(vertices > 23000, "full cloud and rolling band mesh work missing: {vertices}");
}
fn corr_sample() -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    let r = corr_report();
    for i in 0..20 {
        finite(&ctx, corr_frame(&ctx, &r, 1440., false, i));
    }
    let mut result = BTreeMap::new();
    for hover in [false, true] {
        let mode = if hover { "hover" } else { "steady" };
        if hover {
            for i in 140..160 {
                finite(&ctx, corr_frame(&ctx, &r, 1440., true, i));
            }
        }
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            let start = Instant::now();
            let mut out = corr_frame(&ctx, &r, 1440., hover, i + if hover { 160 } else { 20 });
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            if i == 60 {
                corr_guard(&out);
                if hover {
                    assert!(
                        text(&out.shapes).contains("corr.coefficient.beta"),
                        "hover did not paint coefficient MathCard"
                    );
                }
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
                times.entry(format!("paint.studio_corr.{mode}.{metric}")).or_default().push(v);
            }
        }
        for (id, mut samples) in times {
            result.insert(id, tr_perf::summarize(&mut samples));
        }
    }
    result
}
#[test]
fn corr_cards_modes_and_finite_geometry() {
    let r = corr_report();
    for mode in 0..4 {
        for coefficient in 0..2 {
            let card = super::studio_corr::inference_card(&r, mode, coefficient);
            let txt = card.to_text();
            for word in ["corr.coefficient.", "CLOSED THRU", "m=2", "SE method", "95% pointwise CI", "BY q"] {
                assert!(txt.contains(word), "missing {word}: {txt}");
            }
            assert_eq!(card.n.unwrap().0, 5000);
            assert!(card.source.is_some() && card.span.is_some() && card.closed_thru.is_some());
        }
    }
    for width in [320., 1440.] {
        let ctx = egui::Context::default();
        let out = corr_frame(&ctx, &r, width, false, 0);
        if width > 1000. {
            corr_guard(&out);
        }
        finite(&ctx, out);
    }
    let ctx = egui::Context::default();
    for i in 140..166 {
        let out = corr_frame(&ctx, &r, 1440., i > 140, i);
        if i == 165 {
            assert!(text(&out.shapes).contains("corr.coefficient.beta"));
        }
        finite(&ctx, out);
    }
}

fn regime_report() -> tr_data::studio_dist::Report {
    tr_data::studio_dist::compute(
        "AAPL",
        &tr_perf::fixtures::regime_walk(5000),
        tr_core::Timeframe::D1,
        tr_data::studio_dist::Window::All,
        "benchmark fixture only",
        None,
    )
    .unwrap()
}
fn regime_frame(
    ctx: &egui::Context,
    r: &tr_data::studio_dist::Report,
    rotation: &tr_data::studio_regime::RegimeRotation,
    width: f32,
    hover: bool,
    index: usize,
) -> egui::FullOutput {
    let pointer = if hover {
        ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("regime-probe"))).unwrap_or(Rect::NOTHING).center()
            + vec2(if index >= 160 { (index % 2) as f32 } else { 0. }, 0.)
    } else {
        pos2(15., 15.)
    };
    let events = if ctx.input(|i| i.pointer.hover_pos()) == Some(pointer) {
        vec![]
    } else {
        vec![egui::Event::PointerMoved(pointer)]
    };
    let mut out = ctx.run_ui(
        egui::RawInput {
            time: Some(1. + index as f64 * 0.2),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1600.))),
            events,
            ..Default::default()
        },
        |ui| {
            let cell = super::studio_dist::regimes::view(ui, r, Some(rotation), false, &mut 2);
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("regime-probe"), cell));
        },
    );
    out.textures_delta.clear();
    out
}
fn regime_guard(out: &egui::FullOutput) {
    let txt = text(&out.shapes);
    for word in [
        "REGIME FINGERPRINTS",
        "Calm",
        "Normal",
        "Stressed",
        "CLOSED THRU",
        "Δμ bp",
        "HAC Bartlett L=20",
        "mean family m=3",
        "distance family m=3",
        "499 rotations",
        "P(r>0)",
    ] {
        assert!(txt.contains(word), "missing {word}: {txt}");
    }
    let bins=out.shapes.iter().filter(|s|matches!(&s.shape,egui::Shape::Rect(r) if r.fill==super::studio_dist::regime_color(0) || r.fill==super::studio_dist::regime_color(1) || r.fill==super::studio_dist::regime_color(2))).count();
    assert!(bins >= 144, "missing bin rectangles: {bins}");
}
fn regime_sample() -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    let r = regime_report();
    let rotation = r.regime.rotation_input.as_ref().unwrap().compute();
    assert_eq!(rotation.draws, 499);
    assert!(rotation.p.iter().all(Option::is_some));
    assert!(rotation.q[2] <= 0.10);
    for i in 0..20 {
        finite(&ctx, regime_frame(&ctx, &r, &rotation, 1440., false, i));
    }
    let mut result = BTreeMap::new();
    for hover in [false, true] {
        let mode = if hover { "hover" } else { "steady" };
        if hover {
            for i in 140..160 {
                finite(&ctx, regime_frame(&ctx, &r, &rotation, 1440., true, i));
            }
        }
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            let start = Instant::now();
            let mut out = regime_frame(&ctx, &r, &rotation, 1440., hover, i + if hover { 160 } else { 20 });
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            if i == 60 {
                regime_guard(&out);
                if hover {
                    assert!(
                        text(&out.shapes).contains("dist.regime.mean_contrast"),
                        "hover did not paint scale MathCard"
                    );
                }
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
                times.entry(format!("paint.studio_regime.{mode}.{metric}")).or_default().push(v);
            }
        }
        for (id, mut samples) in times {
            result.insert(id, tr_perf::summarize(&mut samples));
        }
    }
    result
}
#[test]
fn regimes_view_finite_geometry_320_1440() {
    let r = regime_report();
    let rotation = r.regime.rotation_input.as_ref().unwrap().compute();
    for width in [320., 1440.] {
        let ctx = egui::Context::default();
        let out = regime_frame(&ctx, &r, &rotation, width, false, 0);
        if width > 1000. {
            regime_guard(&out);
        }
        finite(&ctx, out);
        let mut sparse = r.clone();
        sparse.regime = tr_data::studio_regime::fingerprints(&[], &[], 1);
        finite(&ctx, regime_frame(&ctx, &sparse, &rotation, width, false, 1));
    }
    let ctx = egui::Context::default();
    for i in 140..166 {
        let out = regime_frame(&ctx, &r, &rotation, 1440., i > 140, i);
        if i == 165 {
            assert!(text(&out.shapes).contains("dist.regime.mean_contrast"));
        }
        finite(&ctx, out);
    }
}
#[test]
fn regime_cards_print_method_family_and_reason() {
    use super::studio_dist::regimes::*;
    let r = regime_report();
    let rot = r.regime.rotation_input.as_ref().unwrap().compute();
    for g in 0..3 {
        let cards = [
            mass_card(&r, g, 0),
            quantiles_card(&r, g),
            mean_card(&r, g),
            distance_card(&r, g, Some(&rot)),
            up_card(&r, g),
            tails_card(&r, g),
        ];
        for card in cards {
            let t = card.to_text();
            assert!(t.contains("dist.regime.") && t.contains("CLOSED THRU"));
            assert!(card.n.is_some() && card.span.is_some() && card.source.is_some() && card.provenance.is_some());
        }
        assert!(mean_card(&r, g).to_text().contains("BY q, m=3 (mean family)"));
        assert!(distance_card(&r, g, Some(&rot)).to_text().contains("BY q, m=3 (distance family)"));
        assert!(up_card(&r, g).to_text().contains("Wilson 95%"));
    }
    let mut sparse = r.clone();
    sparse.regime = tr_data::studio_regime::fingerprints(&[Some((0, 0.1)), Some((1, -0.1))], &[false; 2], 1);
    assert!(mean_card(&sparse, 0).to_text().contains("LOW N"));
    assert!(distance_card(&sparse, 0, None).to_text().contains("LOW N"));
    assert!(quantiles_card(&sparse, 2).unavailable.is_some());
    assert!(tails_card(&sparse, 2).unavailable.is_some());
}

fn shape_frame(
    ctx: &egui::Context,
    r: &tr_data::studio_dist::Report,
    shape: &tr_data::studio_shape::Shape,
    intervals: &tr_data::studio_shape::Intervals,
    width: f32,
    hover: bool,
    index: usize,
) -> egui::FullOutput {
    let pointer = if hover {
        ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("shape-probe"))).unwrap_or(Rect::NOTHING).center()
            + vec2(if index >= 160 { (index % 2) as f32 } else { 0. }, 0.)
    } else {
        pos2(15., 15.)
    };
    let events = if ctx.input(|i| i.pointer.hover_pos()) == Some(pointer) {
        vec![]
    } else {
        vec![egui::Event::PointerMoved(pointer)]
    };
    let mut out = ctx.run_ui(
        egui::RawInput {
            time: Some(1. + index as f64 * 0.2),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1600.))),
            events,
            ..Default::default()
        },
        |ui| {
            let cell = super::studio_dist::shape::view(ui, r, shape, Some(intervals));
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("shape-probe"), cell));
        },
    );
    out.textures_delta.clear();
    out
}
fn shape_guard(out: &egui::FullOutput) {
    let txt = text(&out.shapes);
    for word in [
        "DISTRIBUTION SHAPE",
        "CLOSED THRU",
        "HISTOGRAM",
        "KDE",
        "ROLLING QUANTILE RIVER",
        "BOTH TAILS",
        "Loss 99 ES",
        "Gain 99 ES",
        "central probability mass",
        "YEAR STRIP",
        "ABSOLUTE MOVE THRESHOLDS",
    ] {
        assert!(txt.contains(word), "missing {word}: {txt}");
    }
    let histogram_bins = out
        .shapes
        .iter()
        .filter(|s| matches!(&s.shape,egui::Shape::Rect(r) if r.fill==crate::theme::series(0).gamma_multiply(0.35)))
        .count();
    let rectangles = out.shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::Rect(_))).count();
    assert!(histogram_bins >= 12, "missing histogram geometry");
    assert!(rectangles > 1000, "missing river geometry: {rectangles}");
}
fn shape_sample() -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    let r = regime_report();
    let shape = tr_data::studio_shape::compute(&r, 1, false).unwrap();
    let intervals = shape.bootstrap();
    assert_eq!(intervals.draws, 500);
    assert!(shape.river.len() > 4000);
    for i in 0..20 {
        finite(&ctx, shape_frame(&ctx, &r, &shape, &intervals, 1440., false, i));
    }
    let mut result = BTreeMap::new();
    for hover in [false, true] {
        let mode = if hover { "hover" } else { "steady" };
        if hover {
            for i in 140..160 {
                finite(&ctx, shape_frame(&ctx, &r, &shape, &intervals, 1440., true, i));
            }
        }
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            let start = Instant::now();
            let mut out = shape_frame(&ctx, &r, &shape, &intervals, 1440., hover, i + if hover { 160 } else { 20 });
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            if i == 60 {
                shape_guard(&out);
                if hover {
                    assert!(text(&out.shapes).contains("dist.shape.scale"), "hover did not paint scale MathCard");
                }
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
                times.entry(format!("paint.studio_shape.{mode}.{metric}")).or_default().push(v);
            }
        }
        for (id, mut samples) in times {
            result.insert(id, tr_perf::summarize(&mut samples));
        }
    }
    result
}
#[test]
fn shape_geometry_finite_320_1440() {
    let r = regime_report();
    let s = tr_data::studio_shape::compute(&r, 1, false).unwrap();
    let b = s.bootstrap();
    for width in [320., 1440.] {
        let ctx = egui::Context::default();
        let out = shape_frame(&ctx, &r, &s, &b, width, false, 0);
        if width > 1000. {
            shape_guard(&out);
        }
        finite(&ctx, out);
    }
    let ctx = egui::Context::default();
    ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.);
    for i in 0..180 {
        let out = shape_frame(&ctx, &r, &s, &b, 1440., i > 140, i);
        if i == 179 {
            assert!(text(&out.shapes).contains("dist.shape.scale"));
        }
        finite(&ctx, out);
    }
}

fn shell_frame(
    ctx: &egui::Context,
    pane: &mut super::studio_hub::StudioPane,
    worker: &crate::worker::Worker,
    width: f32,
    hover: bool,
    index: usize,
) -> egui::FullOutput {
    let pointer = if hover {
        ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("studio-mean-probe"))).unwrap_or(Rect::NOTHING).center()
            + vec2(if index >= 160 { (index % 2) as f32 } else { 0. }, 0.)
    } else {
        pos2(15. + (index % 2) as f32, 15.)
    };
    let events = if ctx.input(|i| i.pointer.hover_pos()) == Some(pointer) {
        vec![]
    } else {
        vec![egui::Event::PointerMoved(pointer)]
    };
    let mut out = ctx.run_ui(
        egui::RawInput {
            time: Some(1. + index as f64 * 0.2),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 900.))),
            events,
            ..Default::default()
        },
        |ui| {
            pane.show(ui, 201, worker, false, true);
        },
    );
    out.textures_delta.clear();
    out
}
fn shell_guard(out: &egui::FullOutput) {
    let txt = text(&out.shapes);
    for word in [
        "STUDIO",
        "Distribution",
        "Relate",
        "When",
        "Season",
        "Backtest",
        "CLOSED THRU",
        "MEAN / BAR",
        "STDEV / ANN.",
        "DISTRIBUTION STUDIO",
    ] {
        assert!(txt.contains(word), "missing {word}: {txt}");
    }
    assert!(out.shapes.iter().any(|s| matches!(s.shape, egui::Shape::Callback(_))), "native chart geometry absent");
    assert!(out.shapes.len() > 120, "empty shell content");
}
fn shell_sample() -> BTreeMap<String, Summary> {
    let mut result = BTreeMap::new();
    for mode in ["steady", "hover", "chart_dist"] {
        let ctx = egui::Context::default();
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = super::studio_hub::prepared(&worker, &mut requests, mode == "chart_dist");
        let hover = mode == "hover";
        for i in 0..20 {
            finite(&ctx, shell_frame(&ctx, &mut pane, &worker, 1440., false, i));
        }
        if hover {
            for i in 140..160 {
                finite(&ctx, shell_frame(&ctx, &mut pane, &worker, 1440., true, i));
            }
        }
        while requests.try_recv().is_ok() {}
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            let start = Instant::now();
            let mut out = shell_frame(&ctx, &mut pane, &worker, 1440., hover, i + if hover { 160 } else { 20 });
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            if i == 60 {
                shell_guard(&out);
                crate::chart_view::perf_gate::studio_assert_canvas(&pane.chart, &out, mode == "chart_dist");
                if hover {
                    assert!(text(&out.shapes).contains("dist.overview."), "shell hover did not paint DIST MathCard");
                    assert!(text(&out.shapes).contains("Observation clock:"));
                }
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
                times.entry(format!("paint.studio_shell.{mode}.{metric}")).or_default().push(v);
            }
        }
        // Unchanged accepted lenses must not dispatch any statistical recompute.
        while let Ok(r) = requests.try_recv() {
            assert!(
                !matches!(
                    r,
                    crate::worker::Request::StudioDist { .. }
                        | crate::worker::Request::StudioCorr { .. }
                        | crate::worker::Request::StudioSeason { .. }
                        | crate::worker::Request::Stats { .. }
                        | crate::worker::Request::Backtest { .. }
                        | crate::worker::Request::IntradayBacktest { .. }
                        | crate::worker::Request::StudioShape { .. }
                        | crate::worker::Request::StudioShapeBootstrap { .. }
                ),
                "steady shell dispatched research"
            );
        }
        for (id, mut samples) in times {
            result.insert(id, tr_perf::summarize(&mut samples));
        }
    }
    result
}
#[test]
fn studio_shell_geometry_and_actual_hover_320_1440() {
    for width in [320., 599., 899., 1440.] {
        let ctx = egui::Context::default();
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = super::studio_hub::prepared(&worker, &mut requests, false);
        let out = shell_frame(&ctx, &mut pane, &worker, width, false, 0);
        if width == 1440. {
            shell_guard(&out);
        }
        finite(&ctx, out);
        pane.inspector_open = true;
        finite(&ctx, shell_frame(&ctx, &mut pane, &worker, width, false, 1));
    }
    let ctx = egui::Context::default();
    ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.);
    let (worker, mut requests) = crate::worker::Worker::test_channel();
    let mut pane = super::studio_hub::prepared(&worker, &mut requests, false);
    for i in 0..180 {
        let out = shell_frame(&ctx, &mut pane, &worker, 1440., i > 140, i);
        if i == 179 {
            assert!(text(&out.shapes).contains("dist.overview."));
            assert!(text(&out.shapes).contains("CLOSED THRU"));
        }
        finite(&ctx, out);
    }
}

#[test]
fn studio_shell_integrated_populated_canvas_and_dist() {
    let ctx = egui::Context::default();
    let (worker, mut requests) = crate::worker::Worker::test_channel();
    let mut pane = super::studio_hub::prepared(&worker, &mut requests, true);
    for i in 0..21 {
        let out = shell_frame(&ctx, &mut pane, &worker, 1440., false, i);
        if i == 20 {
            shell_guard(&out);
            crate::chart_view::perf_gate::studio_assert_canvas(&pane.chart, &out, true);
        }
        finite(&ctx, out);
    }
}

fn quick_frame(ctx:&egui::Context,pane:&mut super::studio_hub::StudioPane,w:&crate::worker::Worker,width:f32,hover:bool,index:usize)->egui::FullOutput {
    let point=if hover {ctx.data(|d|d.get_temp::<Rect>(egui::Id::new("canvas-strip-probe"))).unwrap_or(Rect::NOTHING).center()+vec2(if index>=160 {(index%2) as f32}else{0.},0.)}else{pos2(2.+(index%2) as f32,2.)};
    let events=if ctx.input(|i|i.pointer.hover_pos())==Some(point){vec![]}else{vec![egui::Event::PointerMoved(point)]};
    let mut out=ctx.run_ui(egui::RawInput {time:Some(1.+index as f64*0.2),screen_rect:Some(Rect::from_min_size(Pos2::ZERO,vec2(width,900.))),events,..Default::default()},|ui|{pane.show(ui,201,w,false,true);});out.textures_delta.clear();out
}
fn quick_guard(p:&super::studio_hub::StudioPane,out:&egui::FullOutput,hover:bool) {
    let txt=text(&out.shapes);
    for word in ["QUICK","Modes","Base UTC","Distribution","Events","Brush","Shared y","CLOSED THRU","now","n="] {assert!(txt.contains(word),"Quick missing {word}: {txt}");}
    crate::chart_view::studio_canvas::assert_meaningful(&p.chart);
    assert!(out.shapes.iter().any(|s|matches!(s.shape,egui::Shape::Callback(_))),"Quick omitted native GPU commands");
    if hover {for s in ["canvas.strip.midrank","Observation clock","# < now"] {assert!(txt.contains(s),"Quick hover missing MathCard {s}: {txt}");}}
}
fn quick_sample()->BTreeMap<String,Summary> {
    let mut result=BTreeMap::new();
    for hover in [false,true] {
        let ctx=egui::Context::default();let (w,mut rx)=crate::worker::Worker::test_channel();let mut p=super::studio_hub::prepared_quick(&w);
        for i in 0..20 {finite(&ctx,quick_frame(&ctx,&mut p,&w,1440.,false,i));crate::chart_view::studio_canvas::complete_test_requests(&mut p.chart,&mut rx);}
        if hover {for i in 140..160 {finite(&ctx,quick_frame(&ctx,&mut p,&w,1440.,true,i));}}
        while rx.try_recv().is_ok(){}
        crate::chart_view::studio_canvas::assert_perf_workload(&p.chart);
        let mut times:BTreeMap<String,Vec<f64>>=BTreeMap::new();
        for i in 0..120 {
            let start=Instant::now();let mut out=quick_frame(&ctx,&mut p,&w,1440.,hover,i+if hover {160}else{20});let ui_us=start.elapsed().as_secs_f64()*1e6;
            if i==60 {quick_guard(&p,&out,hover);}
            let start=Instant::now();let prim=ctx.tessellate(std::mem::take(&mut out.shapes),out.pixels_per_point);let tess_us=start.elapsed().as_secs_f64()*1e6;
            assert!(!prim.is_empty());std::hint::black_box(prim);
            for (metric,v) in [("ui",ui_us),("tess",tess_us),("total",ui_us+tess_us)] {times.entry(format!("paint.studio_quick.{}.{}",if hover {"hover"}else{"steady"},metric)).or_default().push(v);}
        }
        while let Ok(req)=rx.try_recv(){assert!(!matches!(req,crate::worker::Request::StudioCanvas {..}),"unchanged Quick hover/steady created analytical jobs");}
        for (id,mut values) in times {result.insert(id,tr_perf::summarize(&mut values));}
    }result
}
#[test]
fn studio_quick_geometry_and_actual_hover() {
    for width in [320.,1440.] {
        let ctx=egui::Context::default();let (w,mut rx)=crate::worker::Worker::test_channel();let mut p=super::studio_hub::prepared_quick(&w);
        let out=quick_frame(&ctx,&mut p,&w,width,false,0);quick_guard(&p,&out,false);finite(&ctx,out);crate::chart_view::studio_canvas::complete_test_requests(&mut p.chart,&mut rx);
        let mut last=None;for i in 140..160 {last=Some(quick_frame(&ctx,&mut p,&w,width,true,i));}
        let out=last.unwrap();quick_guard(&p,&out,true);finite(&ctx,out);
    }
}

// ---- Studio slice 3: When lens and shaded native chart ----

fn when_pane(sessions: usize) -> super::studio_when::WhenPane {
    let r = super::studio_when::fixture_report(sessions);
    let mut p = super::studio_when::WhenPane::new("AAPL US Equity", tr_core::Timeframe::M5, "RSI(14)<45", 0., 6);
    p.loading = true;
    p.accept(p.generation, Ok(r.clone()));
    p.boot_loading = true;
    let boot = super::studio_when::cached_bootstrap(&r).unwrap();
    p.accept_bootstrap(p.generation, r.report.key, Ok(boot));
    assert!(p.report.is_some() && p.bootstrap.is_some());
    p
}
fn when_frame(
    ctx: &egui::Context,
    p: &mut super::studio_when::WhenPane,
    width: f32,
    hover: bool,
    index: usize,
) -> egui::FullOutput {
    let pointer = if hover {
        ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("when-probe"))).unwrap_or(Rect::NOTHING).center()
            + vec2(if index >= 160 { (index % 2) as f32 } else { 0. }, 0.)
    } else {
        pos2(15. + (index % 2) as f32, 1150.)
    };
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
            let r = p.report.clone().unwrap();
            let boot = p.bootstrap.clone();
            let probe = super::studio_when::view(ui, &r, boot.as_deref(), None, &mut p.horizon);
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("when-probe"), probe));
        },
    );
    out.textures_delta.clear();
    out
}
fn when_guard(p: &super::studio_when::WhenPane, out: &egui::FullOutput, hover: bool) {
    let txt = text(&out.shapes);
    for word in [
        "FIXED FAMILY m=8",
        "Δ mean bp",
        "q mean / event",
        "raw n / episodes / n_eff",
        "NORMALISED SHARED-BIN",
        "Rule n=",
        "Complement n=",
        "P(R>0bp | rule)",
        "block 95%",
        "PRIOR-ONLY",
        "Q5",
        "CLOSED THRU",
    ] {
        assert!(txt.contains(word), "When missing {word}: {txt}");
    }
    let r = &p.report.as_ref().unwrap().report;
    // 120 sessions incl. two half days = 9,288 expected slots; every horizon
    // keeps its completed in-session outcomes (slots − sessions·h − warm-up).
    assert_eq!(r.mask.ts.len(), 9288);
    for c in &r.cells {
        let complete = r.mask.ts.len() - r.sessions * c.horizon;
        assert!(c.raw_n <= complete && c.raw_n + 20 >= complete && c.mean_contrast.p.is_some());
    }
    assert!(out.shapes.len() > 100, "empty When geometry");
    if hover {
        for word in ["WHEN MEAN CONTRAST", "Bartlett lag", "observation clock", "fixed m=8", "CLOSED THRU"] {
            assert!(txt.contains(word), "When hover missing MathCard {word}: {txt}");
        }
    }
}
fn when_sample() -> BTreeMap<String, Summary> {
    let mut result = BTreeMap::new();
    for hover in [false, true] {
        let ctx = egui::Context::default();
        let mut p = when_pane(120);
        for i in 0..20 {
            finite(&ctx, when_frame(&ctx, &mut p, 1440., false, i));
        }
        if hover {
            for i in 140..160 {
                finite(&ctx, when_frame(&ctx, &mut p, 1440., true, i));
            }
        }
        let mode = if hover { "hover" } else { "steady" };
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            let start = Instant::now();
            let mut out = when_frame(&ctx, &mut p, 1440., hover, i + if hover { 160 } else { 20 });
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            if i == 60 {
                when_guard(&p, &out, hover);
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
                times.entry(format!("paint.studio_when.{mode}.{metric}")).or_default().push(v);
            }
        }
        for (id, mut samples) in times {
            result.insert(id, tr_perf::summarize(&mut samples));
        }
    }
    result.extend(when_chart_sample());
    result
}

/// Studio shell on the 5,000-bar native AAPL daily chart with an accepted
/// When report: native GPU chart + accent rule wash + When lens.
fn when_chart_pane(
    worker: &crate::worker::Worker,
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<crate::worker::Request>,
) -> super::studio_hub::StudioPane {
    super::studio_hub::prepared_when(worker, requests)
}
fn when_chart_guard(pane: &super::studio_hub::StudioPane, out: &egui::FullOutput) {
    let txt = text(&out.shapes);
    for word in ["RULE · RSI(14) < 30", "FIXED FAMILY m=8", "CLOSED THRU"] {
        assert!(txt.contains(word), "shaded chart missing {word}: {txt}");
    }
    assert!(when_washes(out) >= 3, "visible accepted rule runs must be shaded");
    crate::chart_view::perf_gate::studio_assert_canvas(&pane.chart, out, false);
}
fn when_washes(out: &egui::FullOutput) -> usize {
    let wash = crate::theme::accent().gamma_multiply(0.08);
    out.shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == wash && r.rect.width() > 0.)).count()
}
fn when_chart_sample() -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    let (worker, mut requests) = crate::worker::Worker::test_channel();
    let mut pane = when_chart_pane(&worker, &mut requests);
    for i in 0..20 {
        finite(&ctx, shell_frame(&ctx, &mut pane, &worker, 1440., false, i));
    }
    while requests.try_recv().is_ok() {}
    let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for i in 0..120 {
        let start = Instant::now();
        let mut out = shell_frame(&ctx, &mut pane, &worker, 1440., false, i + 20);
        let ui_us = start.elapsed().as_secs_f64() * 1e6;
        if i == 60 {
            when_chart_guard(&pane, &out);
        }
        let start = Instant::now();
        let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
        let tess_us = start.elapsed().as_secs_f64() * 1e6;
        assert!(!primitives.is_empty());
        std::hint::black_box(primitives);
        for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
            times.entry(format!("paint.studio_when.chart_shaded.{metric}")).or_default().push(v);
        }
    }
    while let Ok(r) = requests.try_recv() {
        assert!(
            !matches!(r, crate::worker::Request::StudioWhen { .. } | crate::worker::Request::StudioWhenBootstrap { .. }),
            "steady shaded chart dispatched When work"
        );
    }
    times.into_iter().map(|(id, mut s)| (id, tr_perf::summarize(&mut s))).collect()
}
#[test]
fn when_geometry_finite_and_actual_hover_320_1440() {
    for width in [320., 1440.] {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.);
        let mut p = when_pane(40);
        let out = when_frame(&ctx, &mut p, width, false, 0);
        finite(&ctx, out);
        let mut last = None;
        for i in 140..160 {
            last = Some(when_frame(&ctx, &mut p, width, true, i));
        }
        let out = last.unwrap();
        if width == 1440. {
            let txt = text(&out.shapes);
            assert!(txt.contains("WHEN MEAN CONTRAST") && txt.contains("CLOSED THRU"), "{txt}");
        }
        finite(&ctx, out);
    }
}
#[test]
fn when_shaded_native_chart_paints_accepted_mask_only() {
    let ctx = egui::Context::default();
    let (worker, mut requests) = crate::worker::Worker::test_channel();
    let mut pane = when_chart_pane(&worker, &mut requests);
    let mut out = None;
    for i in 0..21 {
        out = Some(shell_frame(&ctx, &mut pane, &worker, 1440., false, i));
    }
    let out = out.unwrap();
    when_chart_guard(&pane, &out);
    finite(&ctx, out);
    // A refusal for the next request clears the wash and its label.
    let p = pane.when_mut().unwrap();
    p.request(201, &worker);
    let g = p.generation;
    p.accept(g, Err("When: sample mode has no research data".into()));
    let out = shell_frame(&ctx, &mut pane, &worker, 1440., false, 30);
    assert!(!text(&out.shapes).contains("RULE · "), "refused report must not shade");
    assert_eq!(when_washes(&out), 0);
}
