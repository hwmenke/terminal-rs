//! Immutable clock evidence presentation. No inference or history scans in paint.
use crate::{
    theme,
    widgets::{HeatScale, MathCard, MathCardExt},
};
use egui::{Align2, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use tr_data::{
    studio_inference::MIN_N,
    studio_season::{SeasonEvidence, SeasonEvidenceCell, SeasonReport, WEEKDAYS},
};

fn bp(v: Option<f64>) -> String {
    v.map(|v| format!("{:+.2} bp", v * 1e4)).unwrap_or_else(|| "—".into())
}
fn ts(t: i64) -> String {
    chrono::DateTime::from_timestamp(t, 0).unwrap_or_default().format("%Y-%m-%d %H:%M UTC").to_string()
}
fn clock(r: &SeasonReport, k: usize) -> String {
    let m = r.clock[k].minute;
    format!("{:02}:{:02}", m / 60, m % 60)
}
pub(in crate::app) fn scale(e: &SeasonEvidence) -> HeatScale {
    HeatScale::diverging(e.heat_scale * 1e4, "Mean bp", |v| format!("{v:.2} bp")).nice()
}
/// Significance controls fill strength, never hue. Sparse descriptive means remain visible as numbers.
pub(in crate::app) fn filled(c: &SeasonEvidenceCell, evidence: bool) -> bool {
    if evidence {
        c.inference.p.is_some() && c.q <= 0.10
    } else {
        c.inference.estimate.is_some()
    }
}
pub(in crate::app) fn card(r: &SeasonReport, row: usize, k: usize) -> MathCard {
    let e = r.evidence.as_ref().unwrap();
    let c = e.cell(row, k);
    let i = &c.inference;
    let group = if row == 0 { "ALL" } else { WEEKDAYS[row - 1] };
    let interval = i.interval
        .map(|(lo, hi)| format!("{} … {}", bp(Some(lo)), bp(Some(hi))))
        .unwrap_or_else(|| "unavailable".into());
    let card = MathCard::new(
        "SEASON CLOCK MEAN",
        "R=C/O−1; mean=ΣR/n; u[s]=I[s](R[s]−mean)/n; Var=Σu²+2Σ(l=1…L)(1−l/(L+1))Σu[s]u[s−l]",
    )
    .provenance(r.provenance)
    .source(&r.source)
    .span(&r.start, &r.end)
    .closed_thru(ts(e.closed_thru))
    .n(i.n, "closed open-to-close bars, one per selected exchange session")
    .input("row / opening clock", format!("{group} / {} NY", clock(r, k)))
    .input("sum / n", format!("{} / {}", bp(i.estimate.map(|v| v * i.n as f64)), i.n))
    .input("grid / bandwidth", format!("T={} expected sessions; L=min(5,T−1)={}", i.span, i.lag))
    .input(
        "HAC SE / pointwise 95% CI",
        format!("{} / {interval}", i.se.map(|se| format!("{:.4} bp", se * 1e4)).unwrap_or_else(|| "—".into())),
    )
    .input(
        "two-sided normal p / BY q",
        format!("{} / {:.6e}", i.p.map(|p| format!("{p:.6e}")).unwrap_or_else(|| "—".into()), c.q),
    )
    .input("family size", format!("m=6×{}={} (ALL + five weekdays)", e.buckets, e.family_size()))
    .note(format!(
        "Observed {}. Coverage {}/{} bars, {}/{} sessions. Missing sessions retain zero score and lag distance; vault joins reset lag products.",
        ts(r.observed_at), r.bars, e.expected_bars, e.sessions, e.grid_sessions,
    ))
    .note(format!(
        "CI=mean±1.96 SE. Test mean=0, p=2Φ(−|mean/SE|); BY q=min(1,H_m×BH). Fixed m={}; unavailable/LOW N p=1. Inferential display requires n≥{MIN_N}.",
        e.family_size(),
    ))
    .caveat("Asymptotic, five-session Bartlett HAC; pointwise intervals are not simultaneous curve coverage. BY gates mean tests only and cannot repair invalid marginal p-values. Repeated symbol/date exploration and source corporate-action limits remain.");
    // Keep descriptive n/mean visible even when inference is unavailable.
    let card = if let Some(reason) = &i.unavailable {
        card.note(format!("Inference unavailable: {reason}; p/CI unavailable, BY input p=1."))
    } else {
        card
    };
    match i.estimate {
        Some(v) => card.value(bp(Some(v))),
        None => card
            .note(format!(
                "n=0 · L={} · p=— · q={:.6} · m={} · {} → {} · CLOSED THRU {} · {}",
                i.lag, c.q, e.family_size(), r.start, r.end, ts(e.closed_thru), r.source,
            ))
            .unavailable("no closed observations in this row and clock"),
    }
}

/// Returns a visible ALL heat cell for tooltip/paint probes.
pub(in crate::app) fn view(ui: &mut Ui, r: &SeasonReport, evidence: &mut bool) -> Rect {
    let Some(e) = &r.evidence else {
        ui.label("Intraday clock evidence unavailable for daily/monthly profiles");
        return Rect::NOTHING;
    };
    let mut probe = Rect::NOTHING;
    ui.label(RichText::new("SEASON EVIDENCE · weekday × New York clock").font(theme::mono(15.)).color(theme::text()));
    ui.label(
        RichText::new(format!("{} · {} · {} · {} … {}", r.symbol, r.provenance.label(), r.timeframe.label(), r.start, r.end))
            .small().color(theme::muted()),
    );
    ui.label(
        RichText::new(format!("{} · observed {} · CLOSED THRU {}", r.source, ts(r.observed_at), ts(e.closed_thru)))
            .small().color(theme::muted()),
    );
    ui.label(format!(
        "Coverage: {}/{} bars · {}/{} sessions · fixed family m={} (6×{})",
        r.bars, e.expected_bars, e.sessions, e.grid_sessions, e.family_size(), e.buckets,
    ));
    super::super::studio_dist::gap_warning(ui, &r.gaps);
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(evidence, false, "Descriptive");
        ui.selectable_value(evidence, true, "Evidence");
    });
    ui.label(
        RichText::new(if *evidence {
            "EVIDENCE · filled only for asymptotic BY q ≤ 0.10 · corner tick: LOW N (<30)"
        } else {
            "DESCRIPTIVE · all observed means filled; no evidence implied · corner tick: LOW N (<30)"
        }).small().color(theme::muted()),
    );
    ScrollArea::vertical().id_salt("season-evidence-scroll").show(ui, |ui| {
        let hs = scale(e);
        hs.key(ui);
        ScrollArea::horizontal().id_salt("season-evidence-heat").show(ui, |ui| {
            let width = ui.available_width().max(64. + e.buckets as f32 * 46.);
            let (rect, _) = ui.allocate_exact_size(vec2(width, 328.), Sense::hover());
            let w = (width - 64.) / e.buckets as f32;
            for k in 0..e.buckets {
                ui.painter().text(
                    pos2(rect.left() + 64. + (k as f32 + 0.5) * w, rect.top() + 12.),
                    Align2::CENTER_CENTER, clock(r, k), theme::mono(9.), theme::muted(),
                );
            }
            for row in 0..6 {
                let label = if row == 0 { "ALL" } else { WEEKDAYS[row - 1] };
                ui.painter().text(
                    pos2(rect.left(), rect.top() + 50. + row as f32 * 49.),
                    Align2::LEFT_CENTER, label, theme::mono(11.), theme::text(),
                );
                for k in 0..e.buckets {
                    let c = e.cell(row, k);
                    let i = &c.inference;
                    let rc = Rect::from_min_size(
                        pos2(rect.left() + 64. + k as f32 * w, rect.top() + 28. + row as f32 * 49.),
                        vec2(w - 2., 45.),
                    );
                    ui.painter().rect_filled(rc, 2., theme::control());
                    // Separate heat strip leaves signed numeric ink on a neutral background.
                    let heat = Rect::from_min_size(rc.min, vec2(rc.width(), 8.));
                    ui.painter().rect_filled(heat, 1., hs.fill_gated(i.estimate.map(|v| v * 1e4), filled(c, *evidence)));
                    if i.p.is_some() && c.q <= 0.10 {
                        ui.painter().rect_stroke(rc, 2., Stroke::new(1., theme::text()), StrokeKind::Inside);
                    }
                    if i.n < MIN_N {
                        ui.painter().line_segment(
                            [rc.left_top() + vec2(2., 13.), rc.left_top() + vec2(6., 9.)],
                            Stroke::new(2., theme::caution()),
                        );
                    }
                    ui.painter().text(
                        rc.center_top() + vec2(0., 21.), Align2::CENTER_CENTER,
                        i.estimate.map(|v| format!("{:+.2}", v * 1e4)).unwrap_or_else(|| "—".into()),
                        theme::mono(9.), i.estimate.map(theme::sign).unwrap_or_else(theme::muted),
                    );
                    ui.painter().text(
                        rc.center_bottom() - vec2(0., 7.), Align2::CENTER_CENTER,
                        format!("n={}", i.n), theme::mono(8.), theme::muted(),
                    );
                    ui.interact(rc, ui.id().with(("season-evidence", row, k)), Sense::hover())
                        .math_card(|| card(r, row, k));
                    if row == 0 && k == 1 {
                        probe = rc;
                    }
                }
            }
        });
        ribbon(ui, r, e);
        ui.label(RichText::new("Pointwise 95% HAC intervals · BY gates mean tests; neither gives simultaneous coverage of this curve.").small().color(theme::muted()));
        ui.label(RichText::new("Absolute-return and volume-share profiles in Calendar are descriptive magnitudes; they carry no directional evidence.").small().color(theme::muted()));
    });
    probe
}

fn ribbon(ui: &mut Ui, r: &SeasonReport, e: &SeasonEvidence) {
    ui.label(
        RichText::new("ALL CLOCK · mean open → close return · pointwise 95% HAC whiskers (bp)")
            .font(theme::mono(12.)).color(theme::text()),
    );
    let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width().max(210.), 240.), Sense::hover());
    let plot = Rect::from_min_max(outer.min + vec2(58., 12.), outer.max - vec2(8., 30.));
    let y = |v: f64| plot.center().y - (v / e.ribbon_scale) as f32 * plot.height() / 2.;
    for step in -2..=2 {
        let v = e.ribbon_scale * step as f64 / 2.;
        ui.painter().line_segment(
            [pos2(plot.left(), y(v)), pos2(plot.right(), y(v))],
            Stroke::new(if step == 0 { 1.5 } else { 1. }, if step == 0 { theme::border() } else { theme::grid() }),
        );
        ui.painter().text(
            pos2(plot.left() - 4., y(v)), Align2::RIGHT_CENTER,
            format!("{:+.2}", v * 1e4), theme::mono(9.), theme::sign(v),
        );
    }
    let w = plot.width() / e.buckets as f32;
    let mut previous = None;
    for k in 0..e.buckets {
        let i = &e.cell(0, k).inference;
        let x = plot.left() + (k as f32 + 0.5) * w;
        if let Some(v) = i.estimate {
            let point = pos2(x, y(v));
            if let Some(p) = previous {
                ui.painter().line_segment([p, point], Stroke::new(1.5, theme::series(0)));
            }
            previous = Some(point);
            ui.painter().circle_filled(point, 2., theme::series(0));
            if let Some((lo, hi)) = i.interval {
                ui.painter().line_segment([pos2(x, y(lo)), pos2(x, y(hi))], Stroke::new(1., theme::text()));
                for v in [lo, hi] {
                    ui.painter().line_segment([pos2(x - 2., y(v)), pos2(x + 2., y(v))], Stroke::new(1., theme::text()));
                }
            } else if i.n < MIN_N {
                ui.painter().line_segment(
                    [point + vec2(-2., -6.), point + vec2(2., -10.)], Stroke::new(2., theme::caution()),
                );
            }
        } else {
            previous = None;
        }
        if k % (e.buckets / 6).max(1) == 0 || k + 1 == e.buckets {
            ui.painter().text(
                pos2(x, plot.bottom() + 7.), Align2::CENTER_TOP, clock(r, k), theme::mono(9.), theme::muted(),
            );
        }
        ui.interact(
            Rect::from_min_max(pos2(x - w / 2., plot.top()), pos2(x + w / 2., plot.bottom())),
            ui.id().with(("season-ribbon", k)), Sense::hover(),
        ).math_card(|| card(r, 0, k));
    }
}
