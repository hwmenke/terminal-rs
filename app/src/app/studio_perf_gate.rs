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
    best.extend(relate_sample());
    best.extend(fair_value_sample());
    best.extend(lab_sample());
    best.extend(resid_sample());
    best.extend(attr_sample());
    best.extend(source_chip_sample());
    best.extend(numeric_table_sample());
    best.extend(legend_sample());
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
            .chain(relate_sample())
            .chain(fair_value_sample())
            .chain(lab_sample())
            .chain(resid_sample())
            .chain(attr_sample())
            .chain(source_chip_sample())
            .chain(numeric_table_sample())
            .chain(legend_sample())
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
    ] {
        assert!(txt.contains(word), "When missing {word}: {txt}");
    }
    // The source chip now lives in the Studio header only (UX slice 1 fix L4).
    assert!(!txt.contains("Yahoo ·") && !txt.contains(" bars · +"), "When lens body must not repeat the chip");
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
/// UX cleanup slice 1: forty compact source chips per frame (multi-series, long failed-attempt chain
/// hidden on hover). Structured receipts are fixtures built here; nothing synthetic reaches the app.
fn source_chip_frame(ctx: &egui::Context, index: usize) -> egui::FullOutput {
    use tr_data::source_receipt::SourceReceipt;
    let mut yahoo = SourceReceipt::new("yahoo", 24_841, Some(1_763_596_800));
    yahoo.observed_at = Some(1_763_683_200);
    yahoo.attempts = vec![
        "Local research archive: Daily research refused: no history".into(),
        "Whats-News (HTTP / SQLite): no daily history".into(),
    ];
    yahoo.detail = format!("yahoo (native provider) · fallback: {}", yahoo.attempts.join("; "));
    let factor = SourceReceipt::new("yahoo", 9_100, Some(1_763_596_800));
    let receipts = [yahoo, factor];
    let mut out = ctx.run_ui(
        egui::RawInput {
            time: Some(1. + index as f64 * 0.2),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440., 1200.))),
            ..Default::default()
        },
        |ui| {
            let parts = [("SPX", &receipts[0]), ("SPY", &receipts[1])];
            for _ in 0..40 {
                crate::widgets::source_chip(ui, "REAL", &parts, || "legacy".into());
            }
        },
    );
    out.textures_delta.clear();
    out
}
/// Measured numeric table: the Lab coefficient table at 900 (wide grid) and 320 (compact grid) over a
/// real-fit-shaped report built in code; layout, typed formatting and painting all run per frame.
fn numeric_table_frame(
    ctx: &egui::Context,
    report: &tr_data::studio_lab::LabReport,
    st: &mut super::studio_lab::LabUi,
    index: usize,
) -> egui::FullOutput {
    let mut out = ctx.run_ui(
        egui::RawInput {
            time: Some(1. + index as f64 * 0.2),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000., 4000.))),
            ..Default::default()
        },
        |ui| {
            for width in [900., 320.] {
                for k in 0..4 {
                    ui.push_id((width as u32, k), |ui| super::studio_lab::table(ui, report, st, width));
                }
            }
        },
    );
    out.textures_delta.clear();
    out
}
fn numeric_table_sample() -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    let report = super::studio_lab::fixture_report(400);
    assert!(report.fit.is_some());
    let mut st = super::studio_lab::LabUi::default();
    for i in 0..20 {
        finite(&ctx, numeric_table_frame(&ctx, &report, &mut st, i));
    }
    let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for i in 0..120 {
        let start = Instant::now();
        let mut out = numeric_table_frame(&ctx, &report, &mut st, i + 20);
        let ui_us = start.elapsed().as_secs_f64() * 1e6;
        if i == 60 {
            let txt = text(&out.shapes);
            // 8 tables, each with its header and k = 1 + 7 factor rows; compact ones hide z/p/q/VIF.
            assert_eq!(txt.matches("Term").count(), 8, "{txt}");
            assert_eq!(txt.matches("95% CI").count(), 8);
            assert_eq!(txt.matches("click a row for SE").count(), 4, "compact hint");
            assert!(txt.contains("intercept") && txt.contains('\u{2212}'), "typed signs");
            assert!(out.shapes.len() >= 8 * 8, "rows painted: {}", out.shapes.len());
        }
        let start = Instant::now();
        let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
        let tess_us = start.elapsed().as_secs_f64() * 1e6;
        assert!(!primitives.is_empty());
        std::hint::black_box(primitives);
        for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
            times.entry(format!("paint.studio_numeric_table.steady.{metric}")).or_default().push(v);
        }
    }
    times.into_iter().map(|(id, mut s)| (id, tr_perf::summarize(&mut s))).collect()
}
/// One legend owner: OHLC header, SMA20 / SMA50 and the fair-value caption laid out through the
/// shared allocator every frame (relate fixture with real fixture bars; indicators built in code).
fn legend_sample() -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    let (worker, mut requests) = crate::worker::Worker::test_channel();
    let mut pane = super::studio_hub::prepared_relate(&worker, &mut requests);
    pane.chart.studio_test_add_smas();
    for i in 0..20 {
        finite(&ctx, fv_frame(&ctx, &mut pane, &worker, 1440., false, i));
    }
    while requests.try_recv().is_ok() {}
    let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for i in 0..120 {
        let start = Instant::now();
        let mut out = fv_frame(&ctx, &mut pane, &worker, 1440., false, i + 20);
        let ui_us = start.elapsed().as_secs_f64() * 1e6;
        if i == 60 {
            let (rows, disjoint) = pane.chart.studio_test_legend();
            disjoint.unwrap();
            assert!(rows.len() >= 4, "OHLC + SMA20 + SMA50 + caption rows: {rows:?}");
            let txt = text(&out.shapes);
            assert!(txt.contains("Fair value · frozen fit") && txt.contains("SMA"), "{txt}");
        }
        let start = Instant::now();
        let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
        let tess_us = start.elapsed().as_secs_f64() * 1e6;
        assert!(!primitives.is_empty());
        std::hint::black_box(primitives);
        for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
            times.entry(format!("paint.studio_legend.steady.{metric}")).or_default().push(v);
        }
    }
    times.into_iter().map(|(id, mut s)| (id, tr_perf::summarize(&mut s))).collect()
}
fn source_chip_sample() -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    for i in 0..20 {
        finite(&ctx, source_chip_frame(&ctx, i));
    }
    let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for i in 0..120 {
        let start = Instant::now();
        let mut out = source_chip_frame(&ctx, i + 20);
        let ui_us = start.elapsed().as_secs_f64() * 1e6;
        if i == 60 {
            let txt = text(&out.shapes);
            assert_eq!(txt.matches("REAL · Yahoo · thru Nov 20 · 24,841 bars · +1 series").count(), 40, "{txt}");
            assert!(!txt.contains("fallback") && !txt.contains("no daily history"), "no text wall in the chip");
            assert!(out.shapes.len() >= 40, "chips painted");
        }
        let start = Instant::now();
        let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
        let tess_us = start.elapsed().as_secs_f64() * 1e6;
        assert!(!primitives.is_empty());
        std::hint::black_box(primitives);
        for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
            times.entry(format!("paint.studio_source_chip.steady.{metric}")).or_default().push(v);
        }
    }
    times.into_iter().map(|(id, mut s)| (id, tr_perf::summarize(&mut s))).collect()
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

// ---- Relate (slice 4): panel paint + fair-value overlay on the native chart -------------------

fn relate_report() -> tr_data::studio_corr::CorrReport {
    use tr_data::studio::{BetaMode, MatrixMode, Transform};
    let (a, b) = tr_perf::fixtures::related(5000, 0.8, 91);
    let peers: Vec<_> = (0..5)
        .map(|i| tr_data::studio_corr::NamedBars {
            symbol: format!("PEER{i} US Equity"),
            bars: tr_perf::fixtures::daily(5001, 200 + i),
            source: "benchmark fixture only".into(),
            ..Default::default()
        })
        .collect();
    let mut spec = tr_data::studio_corr::RelateSpec::new("QQQ US Equity", Transform::LogReturn { h: 5 });
    spec.beta_mode = BetaMode::Expanding;
    spec.matrix = MatrixMode::Common;
    let r = super::studio_corr::relate_fixture_report("AAPL US Equity", &spec, &a, &b, &peers);
    let rel = r.relate.as_ref().unwrap();
    assert!(rel.fit.is_some() && rel.expanding.len() == 1000 && rel.matrix_common.is_some());
    r
}
fn relate_frame(
    ctx: &egui::Context,
    r: &tr_data::studio_corr::CorrReport,
    width: f32,
    hover: bool,
    index: usize,
) -> egui::FullOutput {
    let pointer = if hover {
        ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("relate-probe"))).unwrap_or(Rect::NOTHING).center()
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
        |ui| super::studio_corr::relate_panel(ui, 17, r),
    );
    out.textures_delta.clear();
    out
}
fn relate_guard(out: &egui::FullOutput) {
    let txt = text(&out.shapes);
    for word in [
        "RELATE · TRANSFORM + FROZEN-FIT FAIR VALUE",
        "log-return",
        "in-sample descriptive",
        "EXPANDING β · descriptive",
        "CORRELATION · COMMON PANEL",
        "same n=",
        "CLOSED THRU",
    ] {
        assert!(txt.contains(word), "missing {word}: {txt}");
    }
    assert!(out.shapes.len() > 40, "empty relate panel");
}
fn relate_sample() -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    let r = relate_report();
    for i in 0..20 {
        finite(&ctx, relate_frame(&ctx, &r, 1440., false, i));
    }
    let mut result = BTreeMap::new();
    for hover in [false, true] {
        let mode = if hover { "hover" } else { "steady" };
        if hover {
            for i in 140..160 {
                finite(&ctx, relate_frame(&ctx, &r, 1440., true, i));
            }
        }
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            let start = Instant::now();
            let mut out = relate_frame(&ctx, &r, 1440., hover, i + if hover { 160 } else { 20 });
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            if i == 60 {
                relate_guard(&out);
                if hover {
                    assert!(
                        text(&out.shapes).contains("relate.beta.expanding"),
                        "hover did not paint the expanding-β MathCard"
                    );
                }
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
                times.entry(format!("paint.studio_relate.{mode}.{metric}")).or_default().push(v);
            }
        }
        for (id, mut samples) in times {
            result.insert(id, tr_perf::summarize(&mut samples));
        }
    }
    result
}
#[test]
fn relate_panel_geometry_and_actual_hover_320_1440() {
    let r = relate_report();
    for width in [320., 1440.] {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.);
        let out = relate_frame(&ctx, &r, width, false, 0);
        if width == 1440. {
            relate_guard(&out);
        }
        finite(&ctx, out);
        let mut last = None;
        for i in 140..160 {
            last = Some(relate_frame(&ctx, &r, width, true, i));
        }
        let out = last.unwrap();
        if width == 1440. {
            let txt = text(&out.shapes);
            assert!(txt.contains("relate.beta.expanding") && txt.contains("descriptive"), "{txt}");
        }
        finite(&ctx, out);
    }
}
#[test]
fn relate_fit_cards_and_unavailable_reason() {
    let r = relate_report();
    let rel = r.relate.as_ref().unwrap();
    let fit = rel.fit.as_ref().unwrap();
    // The cards carry the stable ids, the training receipt and the withheld-inference caveat.
    for (c, id) in [(0, "relate.fit.alpha"), (1, "relate.fit.beta")] {
        let card = super::studio_corr::fit_card(&r, rel, fit, c);
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| card.show(ui));
        out.textures_delta.clear();
        let txt = text(&out.shapes);
        assert!(txt.contains(id) && txt.contains("CLOSED THRU") && txt.contains("p and q are withheld"), "{txt}");
    }
    // An unavailable fit prints its reason instead of numbers and paints no overlay inputs.
    let mut bad = r.clone();
    let rel = bad.relate.as_mut().unwrap();
    rel.fit = None;
    rel.points.clear();
    rel.unavailable = Some("Fair value needs ≥30 training pairs; have 3 of 8 valid transformed pairs.".into());
    let ctx = egui::Context::default();
    let out = relate_frame(&ctx, &bad, 1440., false, 0);
    assert!(text(&out.shapes).contains("Fair value unavailable: Fair value needs ≥30 training pairs"));
}

/// Pointer for the fair-value frames: the chart plot centre when hovering; the time-axis row when not.
fn fv_frame(
    ctx: &egui::Context,
    pane: &mut super::studio_hub::StudioPane,
    worker: &crate::worker::Worker,
    width: f32,
    hover: bool,
    index: usize,
) -> egui::FullOutput {
    let pointer = if hover {
        pos2(width * 0.6 + if index >= 160 { (index % 2) as f32 } else { 0. }, 350.)
    } else {
        pos2(width * 0.5 + (index % 2) as f32, 897.)
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
/// Meshes carrying the overlay's band fills (accent at 0.10 / 0.18, halved in-sample).
fn fv_band_meshes(out: &egui::FullOutput) -> Vec<std::sync::Arc<egui::epaint::Mesh>> {
    let accent = crate::theme::series(0);
    let colours = [0.10f32, 0.18, 0.05, 0.09].map(|a| accent.gamma_multiply(a));
    out.shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Mesh(m)
                if !m.vertices.is_empty() && m.vertices.iter().all(|v| colours.contains(&v.color)) =>
            {
                Some(m.clone())
            }
            _ => None,
        })
        .collect()
}
fn fv_strips(out: &egui::FullOutput) -> usize {
    // Each strip is two quads (±2σ, ±1σ) of four vertices.
    fv_band_meshes(out).iter().map(|m| m.vertices.len() / 8).sum()
}
fn fv_guard(pane: &super::studio_hub::StudioPane, out: &egui::FullOutput, hover: bool) {
    let txt = text(&out.shapes);
    // The default viewport shows the latest bars, which are all out of sample; the in-sample note is
    // printed only when an in-sample region is on screen.
    // The caption is short; the fit, thru-date and band definition are on its hover.
    for word in ["Fair value · frozen fit"] {
        assert!(txt.contains(word), "fair-value overlay missing {word}: {txt}");
    }
    assert!(fv_strips(out) > 20, "band fills missing: {}", fv_strips(out));
    crate::chart_view::perf_gate::studio_assert_canvas(&pane.chart, out, false);
    if hover {
        for word in ["relate.fair_value", "residual band", "mean CI", "prediction", "CLOSED THRU"] {
            assert!(txt.contains(word), "fair-value hover missing {word}: {txt}");
        }
    }
}
fn fair_value_sample() -> BTreeMap<String, Summary> {
    let mut result = BTreeMap::new();
    for hover in [false, true] {
        let mode = if hover { "hover" } else { "steady" };
        let ctx = egui::Context::default();
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = super::studio_hub::prepared_relate(&worker, &mut requests);
        for i in 0..20 {
            finite(&ctx, fv_frame(&ctx, &mut pane, &worker, 1440., false, i));
        }
        if hover {
            for i in 140..160 {
                finite(&ctx, fv_frame(&ctx, &mut pane, &worker, 1440., true, i));
            }
        }
        while requests.try_recv().is_ok() {}
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            let start = Instant::now();
            let mut out = fv_frame(&ctx, &mut pane, &worker, 1440., hover, i + if hover { 160 } else { 20 });
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            if i == 60 {
                fv_guard(&pane, &out, hover);
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
                times.entry(format!("paint.studio_fair_value.{mode}.{metric}")).or_default().push(v);
            }
        }
        // A steady accepted overlay never dispatches research.
        while let Ok(r) = requests.try_recv() {
            assert!(!matches!(r, crate::worker::Request::StudioCorr { .. }), "steady fair value dispatched CORR work");
        }
        for (id, mut samples) in times {
            result.insert(id, tr_perf::summarize(&mut samples));
        }
    }
    result
}
#[test]
fn fair_value_overlay_paints_bands_and_actual_hover() {
    let ctx = egui::Context::default();
    let (worker, mut requests) = crate::worker::Worker::test_channel();
    let mut pane = super::studio_hub::prepared_relate(&worker, &mut requests);
    let mut out = None;
    for i in 0..21 {
        out = Some(fv_frame(&ctx, &mut pane, &worker, 1440., false, i));
    }
    let out = out.unwrap();
    fv_guard(&pane, &out, false);
    assert!(!text(&out.shapes).contains("relate.fair_value"), "no hover card without hover");
    finite(&ctx, out);
    ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.);
    let mut last = None;
    for i in 140..160 {
        last = Some(fv_frame(&ctx, &mut pane, &worker, 1440., true, i));
    }
    let out = last.unwrap();
    fv_guard(&pane, &out, true);
    finite(&ctx, out);
    // The bands toggle hides the fills but keeps the centre line, caption and hover.
    pane.corr_mut().unwrap().relate.as_mut().unwrap().bands = false;
    let out = fv_frame(&ctx, &mut pane, &worker, 1440., false, 200);
    assert_eq!(fv_strips(&out), 0);
    assert!(text(&out.shapes).contains("Fair value · frozen fit"));
}
#[test]
fn fair_value_caption_hover_defines_fit_thru_bands_and_source() {
    let ctx = egui::Context::default();
    let (worker, mut requests) = crate::worker::Worker::test_channel();
    let mut pane = super::studio_hub::prepared_relate(&worker, &mut requests);
    for i in 0..5 {
        fv_frame(&ctx, &mut pane, &worker, 1440., false, i);
    }
    ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.);
    let (rows, _) = pane.chart.studio_test_legend();
    let cap = rows.iter().find(|(t, _)| t.starts_with("Fair value · frozen fit")).expect("caption row").1;
    let mut seen = String::new();
    for i in 0..12 {
        let mut out = ctx.run_ui(
            egui::RawInput {
                time: Some(10. + i as f64 * 0.2),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440., 900.))),
                events: vec![egui::Event::PointerMoved(cap.center())],
                ..Default::default()
            },
            |ui| {
                pane.show(ui, 201, &worker, false, true);
            },
        );
        out.textures_delta.clear();
        seen = text(&out.shapes);
    }
    for word in ["frozen fit thru", "residual ±σ bands", "source"] {
        assert!(seen.contains(word), "caption hover missing {word}: {seen}");
    }
}
#[test]
fn legend_rows_ohlc_sma_and_fair_value_never_intersect_at_widths_and_scales() {
    for ppp in [1.0f32, 2.0] {
        for width in [480., 640., 900., 1440.] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let (worker, mut requests) = crate::worker::Worker::test_channel();
            let mut pane = super::studio_hub::prepared_relate(&worker, &mut requests);
            pane.chart.studio_test_add_smas();
            for i in 0..3 {
                fv_frame(&ctx, &mut pane, &worker, width, false, i);
            }
            let (rows, disjoint) = pane.chart.studio_test_legend();
            disjoint.unwrap_or_else(|e| panic!("{width}@{ppp}: {e}"));
            let find = |p: &str| rows.iter().find(|(t, _)| t.starts_with(p)).map(|(_, r)| *r);
            let ohlc = rows.first().expect("OHLC header row").1;
            let (s20, s50) = (find("SMA(20)").or_else(|| find("SMA 20")).or_else(|| find("SMA20")), find("SMA(50)").or_else(|| find("SMA 50")).or_else(|| find("SMA50")));
            let fair = find("Fair value · frozen fit").expect("fair-value caption row");
            let (s20, s50) = (s20.unwrap_or_else(|| panic!("SMA20 row in {:?}", rows.iter().map(|r| &r.0).collect::<Vec<_>>())), s50.expect("SMA50 row"));
            // Four distinct rows stacked top to bottom: OHLC, SMA20, SMA50, then the fair-value caption.
            for (a, b) in [(ohlc, s20), (s20, s50), (s50, fair)] {
                assert!(!a.intersects(b) && a.top() < b.top(), "{width}@{ppp}: {a:?} vs {b:?}");
            }
        }
    }
}
#[test]
fn fair_value_gap_breaks_split_band_polygons() {
    let ctx = egui::Context::default();
    let (worker, mut requests) = crate::worker::Worker::test_channel();
    let mut pane = super::studio_hub::prepared_relate(&worker, &mut requests);
    let base = fv_frame(&ctx, &mut pane, &worker, 1440., false, 0);
    let strips = fv_strips(&fv_frame(&ctx, &mut pane, &worker, 1440., false, 1));
    assert!(strips > 20 && fv_strips(&base) > 0);
    // Break before a visible point: the strip into it disappears and no polygon spans its x.
    let c = pane.corr_mut().unwrap();
    let mut report = (**c.report.as_ref().unwrap()).clone();
    let rel = report.relate.as_mut().unwrap();
    let k = rel.points.len() - 30;
    let gap_ts = rel.points[k].ts;
    rel.points[k].break_before = true;
    c.report = Some(std::sync::Arc::new(report));
    let out = fv_frame(&ctx, &mut pane, &worker, 1440., false, 2);
    assert_eq!(fv_strips(&out), strips - 1, "one strip is removed at the break");
    let x = pane.chart.studio_test_x_of(gap_ts).expect("break point is on a chart bar");
    let spanning = fv_band_meshes(&out).iter().any(|m| {
        m.vertices.chunks(4).any(|q| {
            let (lo, hi) = q.iter().fold((f32::MAX, f32::MIN), |a, v| (a.0.min(v.pos.x), a.1.max(v.pos.x)));
            lo < x - 0.01 && hi > x + 0.01
        })
    });
    assert!(!spanning, "a band polygon crosses the break at x={x}");
}
#[test]
fn fair_value_clears_on_context_change() {
    let ctx = egui::Context::default();
    let (worker, mut requests) = crate::worker::Worker::test_channel();
    let mut pane = super::studio_hub::prepared_relate(&worker, &mut requests);
    let out = fv_frame(&ctx, &mut pane, &worker, 1440., false, 0);
    assert!(text(&out.shapes).contains("Fair value · frozen fit"));
    let old = pane.corr_mut().unwrap().generation;
    let report = pane.corr_mut().unwrap().report.clone().unwrap();
    pane.set_context_symbol("MSFT US Equity");
    let out = fv_frame(&ctx, &mut pane, &worker, 1440., false, 1);
    assert!(!text(&out.shapes).contains("Fair value · frozen fit"), "overlay must clear on the same frame");
    assert!(fv_band_meshes(&out).is_empty());
    // A late response carrying the old token is ignored by the replacement child.
    pane.ensure_child();
    let c = pane.corr_mut().unwrap();
    c.accept_shared(old, Ok(report));
    assert!(c.report.is_none() && c.fair_value_overlay("AAPL US Equity").is_none());
    assert!(c.fair_value_overlay("MSFT US Equity").is_none());
}
#[test]
fn fair_value_not_painted_in_quick_canvas_mode_or_for_other_inputs() {
    let ctx = egui::Context::default();
    let (worker, mut requests) = crate::worker::Worker::test_channel();
    let mut pane = super::studio_hub::prepared_relate(&worker, &mut requests);
    // Changed inputs: the accepted report no longer matches, so nothing is painted.
    pane.corr_mut().unwrap().relate.as_mut().unwrap().transform = tr_data::studio::Transform::Change { h: 5 };
    let out = fv_frame(&ctx, &mut pane, &worker, 1440., false, 0);
    assert!(!text(&out.shapes).contains("Fair value · frozen fit"));
    pane.corr_mut().unwrap().relate.as_mut().unwrap().transform = tr_data::studio::Transform::LogReturn { h: 5 };
    assert!(text(&fv_frame(&ctx, &mut pane, &worker, 1440., false, 1).shapes).contains("Fair value · frozen fit"));
    // Quick canvas: the y axis is not price, so the overlay must not paint.
    let mut view = pane.chart.canvas_view();
    view.enabled = true;
    pane.chart.set_canvas_view(view);
    let out = fv_frame(&ctx, &mut pane, &worker, 1440., false, 2);
    assert!(!text(&out.shapes).contains("Fair value · frozen fit"));
}
#[test]
fn relate_bands_hover_text_distinguishes_residual_band_from_mean_ci() {
    // n=2,000 on the related fixture: ±1σ band half-width is exactly σ; the mean CI is far narrower.
    use tr_data::studio::Transform;
    use tr_data::studio_corr as sc;
    let (a, b) = tr_perf::fixtures::related(2000, 1.1, 11);
    let named = |s: &str, bars: Vec<tr_core::Bar>| sc::NamedBars {
        symbol: s.into(),
        bars,
        source: "benchmark fixture only".into(),
        ..Default::default()
    };
    let (a, b) = (named("AAPL US Equity", a), named("QQQ US Equity", b));
    let opt = sc::Options { annual_periods: 252., daily: true };
    let p = sc::prepare(&a, &b, opt, tr_core::Timeframe::D1, Transform::Change { h: 1 }, "u", 0, None).unwrap();
    let fit = sc::training_fit(&p, 1, tr_data::studio_inference::CovarianceMethod::HacFixed5).unwrap();
    let pts = sc::fair_value(&p, &fit);
    let q = pts.last().unwrap();
    let band = q.bands[2] - q.centre;
    assert!((band - fit.sigma).abs() <= 1e-12 * fit.sigma.abs().max(1.), "{band} vs σ {}", fit.sigma);
    let h = sc::hover_stats(&fit, q).unwrap();
    let half = (h.mean_ci.unwrap()[1] - q.centre).abs();
    assert!((half - 1.96 * h.se_mean).abs() <= 1e-9 * half.max(1e-12));
    assert!(half < fit.sigma / 5., "mean CI {half} vs σ {}", fit.sigma);
    assert!(h.se_prediction > fit.sigma && h.se_prediction < fit.sigma * 1.01);
    // The hover card text names all three quantities.
    let report = super::studio_corr::relate_fixture_report(
        "AAPL US Equity",
        &sc::RelateSpec::new("QQQ US Equity", Transform::Change { h: 1 }),
        &a.bars,
        &b.bars,
        &[],
    );
    let rel = report.relate.as_ref().unwrap();
    let card = crate::app::studio_corr::fair_value_card(&report, rel, rel.fit.as_ref().unwrap(), rel.points.last().unwrap());
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(Default::default(), |ui| card.show(ui));
    out.textures_delta.clear();
    let txt = text(&out.shapes);
    for word in ["residual band", "mean CI", "prediction", "CLOSED THRU", "relate.fair_value"] {
        assert!(txt.contains(word), "card missing {word}: {txt}");
    }
}

// ---- Regression Lab (slice 5): panel paint, actual hover, LOW N and unavailable states ----------

fn lab_report() -> tr_data::studio_lab::LabReport {
    let (spec, target, rows) = tr_perf::fixtures::lab_model(5000);
    let r = super::studio_lab::fixture_compute(&spec, &target, &rows);
    let fit = r.fit.as_ref().expect("lab fit available");
    assert!(fit.k == 8 && r.low_n.is_none() && r.inference.len() == 4, "{:?}", r.unavailable);
    // Lags (≤5) and horizons (≤5) cost a handful of the 5,000 bars; every complete row is plotted.
    assert!(r.design_rows > 4900 && r.points.len() == r.design_rows && r.diagnostics.is_some() && fit.vif.len() == 7);
    assert!(r.inference.iter().all(|i| i.covariance.covariance.is_some()));
    r
}
fn lab_frame(
    ctx: &egui::Context,
    r: &tr_data::studio_lab::LabReport,
    st: &mut super::studio_lab::LabUi,
    width: f32,
    hover: bool,
    index: usize,
) -> egui::FullOutput {
    let pointer = if hover {
        ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("lab-probe"))).unwrap_or(Rect::NOTHING).center()
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
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1400.))),
            events,
            ..Default::default()
        },
        |ui| super::studio_lab::panel(ui, 23, r, st),
    );
    out.textures_delta.clear();
    out
}
fn lab_guard(out: &egui::FullOutput) {
    let txt = text(&out.shapes);
    for word in [
        "COEFFICIENTS",
        "FITTED vs ACTUAL",
        "R²_OOS vs training mean",
        "in-sample, descriptive",
        "count per cell",
        "VIF",
        "HAC auto",
        "TRAIN",
        "TEST",
        "purged 0",
    ] {
        assert!(txt.contains(word), "missing {word}: {txt}");
    }
    assert!(out.shapes.len() > 60, "empty lab panel");
}
fn lab_sample() -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    let r = lab_report();
    let mut st = super::studio_lab::LabUi::default();
    st.shown_cov = 3;
    for i in 0..20 {
        finite(&ctx, lab_frame(&ctx, &r, &mut st, 1440., false, i));
    }
    let mut result = BTreeMap::new();
    for hover in [false, true] {
        let mode = if hover { "hover" } else { "steady" };
        if hover {
            for i in 140..160 {
                finite(&ctx, lab_frame(&ctx, &r, &mut st, 1440., true, i));
            }
        }
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            let start = Instant::now();
            let mut out = lab_frame(&ctx, &r, &mut st, 1440., hover, i + if hover { 160 } else { 20 });
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            if i == 60 {
                lab_guard(&out);
                if hover {
                    let txt = text(&out.shapes);
                    assert!(
                        txt.contains("lab.fit.coef") && txt.contains("CLOSED THRU"),
                        "hover did not paint the coefficient MathCard"
                    );
                }
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
                times.entry(format!("paint.studio_lab.{mode}.{metric}")).or_default().push(v);
            }
        }
        for (id, mut samples) in times {
            result.insert(id, tr_perf::summarize(&mut samples));
        }
    }
    result
}
#[test]
fn lab_panel_geometry_and_actual_hover_320_1440() {
    let r = lab_report();
    for width in [320., 1440.] {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.);
        let mut st = super::studio_lab::LabUi::default();
        let out = lab_frame(&ctx, &r, &mut st, width, false, 0);
        lab_guard(&out);
        for clipped in &out.shapes {
            assert!(clipped.clip_rect.is_finite());
        }
        finite(&ctx, out);
        let mut last = None;
        for i in 140..160 {
            last = Some(lab_frame(&ctx, &r, &mut st, width, true, i));
        }
        let out = last.unwrap();
        let txt = text(&out.shapes);
        assert!(txt.contains("lab.fit.coef") && txt.contains("CLOSED THRU"), "{width}: {txt}");
        assert!(txt.contains("levels may be nonstationary"), "level caveat missing: {txt}");
        let probe = ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("lab-probe"))).unwrap();
        assert!(probe.width() <= width + 1. && probe.is_finite(), "table row overflows {width}px: {probe:?}");
        finite(&ctx, out);
        // Selecting a diagnostic tab or covariance paints from the same report.
        for (tab, cov) in [(1, 0), (2, 1)] {
            st.diag_tab = tab;
            st.shown_cov = cov;
            finite(&ctx, lab_frame(&ctx, &r, &mut st, width, false, 200 + tab));
        }
    }
}
fn any_shape(shapes: &[egui::epaint::ClippedShape], f: &dyn Fn(&egui::Shape) -> bool) -> bool {
    fn walk(s: &egui::Shape, f: &dyn Fn(&egui::Shape) -> bool) -> bool {
        match s {
            egui::Shape::Vec(v) => v.iter().any(|s| walk(s, f)),
            s => f(s),
        }
    }
    shapes.iter().any(|c| walk(&c.shape, f))
}
#[test]
fn lab_low_n_and_unavailable_states_paint() {
    use tr_data::studio::Transform as T;
    use tr_data::studio_lab::FactorSpec;
    let ctx = egui::Context::default();
    // LOW N: 100 bars leave < max(60, 10·k) = 80 training rows for k = 8; β stays, inference is withheld.
    let (spec, target, rows) = tr_perf::fixtures::lab_model(100);
    let r = super::studio_lab::fixture_compute(&spec, &target, &rows);
    assert!(r.fit.is_some() && r.inference.is_empty() && r.low_n.is_some(), "{:?}", r.unavailable);
    let mut st = super::studio_lab::LabUi::default();
    let out = lab_frame(&ctx, &r, &mut st, 1440., false, 0);
    let txt = text(&out.shapes);
    assert!(txt.contains("LOW N: training rows"), "{txt}");
    assert!(txt.contains("COEFFICIENTS") && txt.contains("withheld"), "{txt}");
    let tick = |s: &egui::Shape| {
        matches!(s, egui::Shape::LineSegment { stroke, .. } if stroke.color == crate::theme::caution())
    };
    assert!(any_shape(&out.shapes, &tick), "LOW N corner tick missing");
    finite(&ctx, out);
    // Unavailable: an exact duplicate factor makes the design rank-deficient. A reason card replaces
    // every fitted plot and the table; no numbers are painted.
    let (mut spec, target, mut rows) = tr_perf::fixtures::lab_model(5000);
    let mut twin = rows[0].clone();
    twin.symbol = "F0B US Equity".into();
    spec.factors[1] = FactorSpec { symbol: twin.symbol.clone(), transform: T::LogReturn { h: 1 }, lag: 0 };
    spec.factors[0].transform = T::LogReturn { h: 1 };
    spec.factors[0].lag = 0;
    rows[1] = twin;
    let r = super::studio_lab::fixture_compute(&spec, &target, &rows);
    assert!(
        r.fit.is_none() && r.unavailable.as_deref().is_some_and(|m| m.contains("rank-deficient")),
        "{:?}",
        r.unavailable
    );
    let out = lab_frame(&ctx, &r, &mut st, 1440., false, 1);
    let txt = text(&out.shapes);
    assert!(txt.contains("REGRESSION LAB UNAVAILABLE") && txt.contains("rank-deficient"), "{txt}");
    assert!(!txt.contains("FITTED vs ACTUAL") && !txt.contains("R²_OOS"), "fitted content painted: {txt}");
    finite(&ctx, out);
}

// ---- Frozen residual tickers (slice 6): panel paint, actual hover, pending / unavailable states ----

type ResidState = (
    std::sync::Arc<tr_data::studio_residual::FrozenFit>,
    std::sync::Arc<tr_data::studio_residual::ResidualReport>,
    std::sync::Arc<tr_data::studio_residual::ResidualChecks>,
);
fn resid_state() -> ResidState {
    let (fit, rep, chk) = super::studio_residual::fixture(5000);
    assert!(rep.unavailable.is_none() && rep.scored.len() > 900, "{:?}", rep.unavailable);
    assert!(rep.residual.ts.len() > 4900 && rep.residual.values.iter().flatten().any(|e| *e < 0.));
    assert!(matches!(chk.adf, tr_data::studio_residual::CheckState::Ready(_)), "{:?}", chk.adf);
    assert!(matches!(&chk.monitor, tr_data::studio_residual::CheckState::Ready(m) if m.p.is_some()), "{:?}", chk.monitor);
    (fit, rep, chk)
}
fn resid_frame(
    ctx: &egui::Context,
    s: &ResidState,
    st: &mut super::studio_residual::ResidUi,
    width: f32,
    hover: bool,
    index: usize,
) -> egui::FullOutput {
    let pointer = if hover {
        ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("resid-probe"))).unwrap_or(Rect::NOTHING).center()
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
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1400.))),
            events,
            ..Default::default()
        },
        |ui| super::studio_residual::panel(ui, 24, &s.0, &s.1, Some(&s.2), false, st),
    );
    out.textures_delta.clear();
    out
}
fn resid_guard(out: &egui::FullOutput) {
    let txt = text(&out.shapes);
    for word in [
        "FROZEN FIT · scored, not refit",
        "TRAIN | SCORED",
        "TRAIN · in-sample",
        "−2σ saved",
        "Stationarity check · ADF",
        "H0 unit root",
        "Change check",
        "SAVED COEFFICIENTS",
        "CLOSED THRU",
    ] {
        assert!(txt.contains(word), "missing {word}: {txt}");
    }
    assert!(out.shapes.len() > 60, "empty residual panel");
}
fn resid_sample() -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    let s = resid_state();
    let mut st = super::studio_residual::ResidUi::default();
    for i in 0..20 {
        finite(&ctx, resid_frame(&ctx, &s, &mut st, 1440., false, i));
    }
    let mut result = BTreeMap::new();
    for hover in [false, true] {
        let mode = if hover { "hover" } else { "steady" };
        if hover {
            for i in 140..160 {
                finite(&ctx, resid_frame(&ctx, &s, &mut st, 1440., true, i));
            }
        }
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            let start = Instant::now();
            let mut out = resid_frame(&ctx, &s, &mut st, 1440., hover, i + if hover { 160 } else { 20 });
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            if i == 60 {
                resid_guard(&out);
                if hover {
                    let txt = text(&out.shapes);
                    assert!(
                        txt.contains("resid.score") && txt.contains("CLOSED THRU"),
                        "hover did not paint the residual MathCard"
                    );
                }
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
                times.entry(format!("paint.studio_residual.{mode}.{metric}")).or_default().push(v);
            }
        }
        for (id, mut samples) in times {
            result.insert(id, tr_perf::summarize(&mut samples));
        }
    }
    result
}
#[test]
fn resid_panel_geometry_and_actual_hover_320_1440() {
    let s = resid_state();
    for width in [320., 1440.] {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|st| st.interaction.tooltip_delay = 0.);
        let mut st = super::studio_residual::ResidUi::default();
        let out = resid_frame(&ctx, &s, &mut st, width, false, 0);
        resid_guard(&out);
        for clipped in &out.shapes {
            assert!(clipped.clip_rect.is_finite());
        }
        finite(&ctx, out);
        let mut last = None;
        for i in 140..160 {
            last = Some(resid_frame(&ctx, &s, &mut st, width, true, i));
        }
        let out = last.unwrap();
        let txt = text(&out.shapes);
        for word in ["resid.score", "CLOSED THRU", "FROZEN FIT · scored, not refit", "TRAIN | SCORED"] {
            assert!(txt.contains(word), "{width}: hover missing {word}: {txt}");
        }
        let probe = ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("resid-probe"))).unwrap();
        assert!(probe.width() <= width + 1. && probe.height() == 220. && probe.is_finite(), "chart overflows {width}px: {probe:?}");
        finite(&ctx, out);
        // The z toggle paints from the same report.
        st.show_z = true;
        finite(&ctx, resid_frame(&ctx, &s, &mut st, width, false, 200));
    }
}
#[test]
fn resid_pending_and_unavailable_states_paint() {
    use tr_data::studio_residual::{CheckState, InputCheck};
    let ctx = egui::Context::default();
    let (fit, rep, _) = resid_state();
    // 23 scored bars: the ADF check is pending, never "stationary"; a revised input shows its badge.
    let mut r = (*rep).clone();
    let keep = 23;
    r.scored.truncate(keep);
    r.scored_ts.truncate(keep);
    r.scored_breaks.truncate(keep);
    r.revision[1].1 = InputCheck::Revised { live: 1, saved: 2 };
    let c = tr_data::studio_residual::checks(&fit, &r);
    assert!(matches!(c.adf, CheckState::Pending { have: 23, need: 60, .. }), "{:?}", c.adf);
    let mut st = super::studio_residual::ResidUi::default();
    let mut out = ctx.run_ui(
        egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440., 1400.))), ..Default::default() },
        |ui| super::studio_residual::panel(ui, 25, &fit, &r, Some(&c), false, &mut st),
    );
    out.textures_delta.clear();
    let txt = text(&out.shapes);
    for word in ["Stationarity check pending: 23/60 scored bars", "INPUT REVISED", "the saved model is unchanged"] {
        assert!(txt.contains(word), "missing {word}: {txt}");
    }
    assert!(!txt.contains("stationary ·") && !txt.contains("ADF τ"), "a pending check painted a verdict: {txt}");
    let tick = |s: &egui::Shape| {
        matches!(s, egui::Shape::LineSegment { stroke, .. } if stroke.color == crate::theme::caution())
    };
    assert!(any_shape(&out.shapes, &tick), "revision badge corner tick missing");
    finite(&ctx, out);
    // Checks still computing: the tiles keep their reserved space.
    let mut out = ctx.run_ui(
        egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440., 1400.))), ..Default::default() },
        |ui| super::studio_residual::panel(ui, 25, &fit, &rep, None, true, &mut st),
    );
    out.textures_delta.clear();
    assert!(text(&out.shapes).contains("computing…"));
    finite(&ctx, out);
    // A missing model paints a recoverable card and no numbers.
    let u = crate::studio_models::ModelUnavailable {
        kind: crate::studio_models::UnavailableKind::Missing,
        id: "gone-v1".into(),
        message: "no such saved model".into(),
    };
    let mut out = ctx.run_ui(
        egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440., 800.))), ..Default::default() },
        |ui| super::studio_residual::model_unavailable(ui, &u),
    );
    out.textures_delta.clear();
    let txt = text(&out.shapes);
    assert!(txt.contains("Model `gone-v1` unavailable: missing"), "{txt}");
    assert!(!txt.contains("σ_saved") && !txt.contains("FROZEN FIT") && !txt.contains("τ"), "numbers painted: {txt}");
    finite(&ctx, out);
    // An unavailable score paints the reason card and no chart.
    let mut bad = (*rep).clone();
    bad.unavailable = Some("Residual scoring needs every saved input; unavailable: F3 (no archive).".into());
    bad.factor_errors = vec![("F3 US Equity".into(), "no archive".into())];
    let mut out = ctx.run_ui(
        egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440., 1400.))), ..Default::default() },
        |ui| super::studio_residual::panel(ui, 25, &fit, &bad, None, false, &mut st),
    );
    out.textures_delta.clear();
    let txt = text(&out.shapes);
    assert!(txt.contains("RESIDUAL UNAVAILABLE") && txt.contains("no archive") && !txt.contains("TRAIN | SCORED"), "{txt}");
    finite(&ctx, out);
}

// ---- Factor attribution (slice 7): panel paint, actual hover, unavailable / LOW N / IN-SAMPLE states ----

type AttrState = (
    std::sync::Arc<tr_data::studio_residual::FrozenFit>,
    std::sync::Arc<tr_data::studio_attribution::AttributionReport>,
);
fn attr_state() -> AttrState {
    let (fit, rep) = super::studio_attribution::fixture(5000);
    let a = tr_data::studio_attribution::attribute(
        &fit,
        &rep,
        &tr_data::studio_attribution::AttrWindow::Obs { obs: 20, to: None },
    );
    assert!(a.unavailable.is_none(), "{:?}", a.unavailable);
    assert!(a.m == 20 && a.factors.len() == 7 && a.covariances.len() == 4 && a.path.len() == 20);
    (fit, std::sync::Arc::new(a))
}
fn attr_frame(
    ctx: &egui::Context,
    fit: &tr_data::studio_residual::FrozenFit,
    a: &tr_data::studio_attribution::AttributionReport,
    st: &mut super::studio_attribution::AttrUi,
    width: f32,
    hover: bool,
    index: usize,
) -> egui::FullOutput {
    let pointer = if hover {
        ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("attr-bar0"))).unwrap_or(Rect::NOTHING).center()
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
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1400.))),
            events,
            ..Default::default()
        },
        |ui| super::studio_attribution::panel(ui, 26, fit, a, st),
    );
    out.textures_delta.clear();
    out
}
fn attr_guard(out: &egui::FullOutput) {
    let txt = text(&out.shapes);
    for word in ["FROZEN FIT · attributed, not refit", "Actual", "Drift", "Residual", "WATERFALL", "Predicted", "CLOSED THRU"] {
        assert!(txt.contains(word), "missing {word}: {txt}");
    }
    assert!(out.shapes.len() > 60, "empty attribution panel");
}
fn attr_sample() -> BTreeMap<String, Summary> {
    let ctx = egui::Context::default();
    let (fit, a) = attr_state();
    let mut st = super::studio_attribution::AttrUi::default();
    for i in 0..20 {
        finite(&ctx, attr_frame(&ctx, &fit, &a, &mut st, 1440., false, i));
    }
    let mut result = BTreeMap::new();
    for hover in [false, true] {
        let mode = if hover { "hover" } else { "steady" };
        if hover {
            for i in 140..160 {
                finite(&ctx, attr_frame(&ctx, &fit, &a, &mut st, 1440., true, i));
            }
        }
        let mut times: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for i in 0..120 {
            let start = Instant::now();
            let mut out = attr_frame(&ctx, &fit, &a, &mut st, 1440., hover, i + if hover { 160 } else { 20 });
            let ui_us = start.elapsed().as_secs_f64() * 1e6;
            if i == 60 {
                attr_guard(&out);
                if hover {
                    let txt = text(&out.shapes);
                    assert!(
                        txt.contains("attr.contribution") && txt.contains("CLOSED THRU"),
                        "hover did not paint the attribution MathCard"
                    );
                }
            }
            let start = Instant::now();
            let primitives = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            let tess_us = start.elapsed().as_secs_f64() * 1e6;
            assert!(!primitives.is_empty());
            std::hint::black_box(primitives);
            for (metric, v) in [("ui", ui_us), ("tess", tess_us), ("total", ui_us + tess_us)] {
                times.entry(format!("paint.studio_attribution.{mode}.{metric}")).or_default().push(v);
            }
        }
        for (id, mut samples) in times {
            result.insert(id, tr_perf::summarize(&mut samples));
        }
    }
    result
}
#[test]
fn attr_panel_geometry_and_actual_hover_320_1440() {
    let (fit, a) = attr_state();
    for width in [320., 1440.] {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|st| st.interaction.tooltip_delay = 0.);
        let mut st = super::studio_attribution::AttrUi::default();
        let out = attr_frame(&ctx, &fit, &a, &mut st, width, false, 0);
        attr_guard(&out);
        for clipped in &out.shapes {
            assert!(clipped.clip_rect.is_finite());
        }
        finite(&ctx, out);
        let mut last = None;
        for i in 140..160 {
            last = Some(attr_frame(&ctx, &fit, &a, &mut st, width, true, i));
        }
        let out = last.unwrap();
        let txt = text(&out.shapes);
        for word in ["attr.contribution", "CLOSED THRU", "FROZEN FIT · attributed, not refit", "Actual", "Residual"] {
            assert!(txt.contains(word), "{width}: hover missing {word}: {txt}");
        }
        let probe = ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("attr-probe"))).unwrap();
        assert!(probe.width() <= width + 1. && probe.height() == 240. && probe.is_finite(), "waterfall overflows {width}px: {probe:?}");
        finite(&ctx, out);
        // Both SE meanings and every stored estimator paint from the same report.
        st.se = tr_data::studio_attribution::AttributionSe::Coefficient;
        st.cov = Some(0);
        finite(&ctx, attr_frame(&ctx, &fit, &a, &mut st, width, false, 200));
    }
}
#[test]
fn attr_states_paint() {
    use tr_data::studio_attribution::WindowLabel;
    let ctx = egui::Context::default();
    let (fit, a) = attr_state();
    let mut st = super::studio_attribution::AttrUi::default();
    let tick = |s: &egui::Shape| {
        matches!(s, egui::Shape::LineSegment { stroke, .. } if stroke.color == crate::theme::caution())
    };
    // An unavailable window paints the reason and no numbers or bars.
    let mut bad = (*a).clone();
    bad.unavailable = Some("window starts before the scored grid (first bar 2026-01-02 00:00 UTC)".into());
    let out = attr_frame(&ctx, &fit, &bad, &mut st, 1440., false, 1);
    let txt = text(&out.shapes);
    assert!(txt.contains("ATTRIBUTION UNAVAILABLE") && txt.contains("starts before the scored grid"), "{txt}");
    assert!(!txt.contains("attr.contribution") && !txt.contains("WATERFALL"), "numbers painted: {txt}");
    finite(&ctx, out);
    // LOW N: the predictive SE is withheld with a caution tick; the coefficient SE stays.
    let mut low = (*a).clone();
    for c in &mut low.covariances {
        c.predictive_se = None;
    }
    low.residual_var.variance = None;
    low.residual_var.unavailable = Some("LOW N: 12 stored residual pairs at lag 20 < 30; predictive SE withheld".into());
    let out = attr_frame(&ctx, &fit, &low, &mut st, 1440., false, 2);
    let txt = text(&out.shapes);
    assert!(txt.contains("withheld") && txt.contains("LOW N"), "{txt}");
    assert!(any_shape(&out.shapes, &tick), "LOW N corner tick missing");
    finite(&ctx, out);
    // IN-SAMPLE is labelled, never refused.
    let mut ins = (*a).clone();
    ins.label = WindowLabel::InSample;
    let out = attr_frame(&ctx, &fit, &ins, &mut st, 1440., false, 3);
    let txt = text(&out.shapes);
    assert!(txt.contains("IN-SAMPLE") && txt.contains("not an out-of-sample error"), "{txt}");
    assert!(any_shape(&out.shapes, &tick), "IN-SAMPLE corner tick missing");
    finite(&ctx, out);
    // A missing model paints a recoverable card and no numbers.
    let u = crate::studio_models::ModelUnavailable {
        kind: crate::studio_models::UnavailableKind::Missing,
        id: "gone-v1".into(),
        message: "no such saved model".into(),
    };
    let mut out = ctx.run_ui(
        egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440., 800.))), ..Default::default() },
        |ui| super::studio_residual::model_unavailable(ui, &u),
    );
    out.textures_delta.clear();
    let txt = text(&out.shapes);
    assert!(txt.contains("Model `gone-v1` unavailable: missing") && !txt.contains("Actual") && !txt.contains("WATERFALL"), "{txt}");
    finite(&ctx, out);
}
