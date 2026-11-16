//! Paint immutable OR reports; selection never triggers inference or history scans.
use crate::{
    theme,
    widgets::{self, HeatScale, MathCard, MathCardExt},
};
use egui::{Align2, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use tr_data::{
    studio_inference::MIN_N,
    studio_season::{OR_FAMILY, OR_HORIZONS, OrCell, OrLens, OrState},
};

#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub state: usize,
    pub clock: usize,
    pub horizon: usize,
}
impl Default for Selection {
    fn default() -> Self {
        Self { state: 0, clock: 1, horizon: 0 }
    }
}
fn bps(v: Option<f64>) -> String {
    v.map(|v| format!("{v:+.2} bp")).unwrap_or_else(|| "—".into())
}
fn clock(minute: u32) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}
fn timestamp(t: i64) -> String {
    chrono::DateTime::from_timestamp(t, 0).unwrap_or_default().format("%Y-%m-%d %H:%M UTC").to_string()
}
fn base(r: &OrLens, c: &OrCell, title: &str, formula: &str) -> MathCard {
    MathCard::new(title, formula)
        .provenance(r.provenance)
        .source(&r.source)
        .span(&r.start, &r.end)
        .closed_thru(timestamp(r.closed_thru))
        .n(c.selected.len() + c.complement.len(), "completed same-clock session outcomes")
        .input("origin close / horizon", format!("{} NY / {} minutes", clock(c.minute), c.horizon * 30))
        .input("group / complement n", format!("{} / {}", c.selected.len(), c.complement.len()))
        .note(format!("Observed {}. TODAY excluded; no overnight or shortened horizons.", timestamp(r.observed_at)))
}
pub(in crate::app) fn effect_card(r: &OrLens, c: &OrCell) -> MathCard {
    let i = &c.inference;
    let card=base(r,c,"OR MEAN CONTRAST", "R = 10⁴(C[t+h]/C[t] − 1); Δ = mean(group) − mean(same-clock complement)")
        .input("condition",format!("{}: C[t] vs exact 09:30–10:00 high/low; equality is Inside",c.state.label()))
        .input("means (bp)",format!("{} − {}; all {}",bps(c.mean),bps(c.complement_mean),bps(c.all_mean)))
        .input("HAC grid / bandwidth",format!("T={} expected sessions; L=min(5,T−1)={}",i.span,i.lag))
        .input("SE / p / BY q",format!("{} / {} / {:.5}",i.se.map(|x|format!("{x:.4} bp")).unwrap_or_else(||"—".into()),i.p.map(|x|format!("{x:.5}")).unwrap_or_else(||"—".into()),c.q))
        .note("u[s]=I[g](R−mean[g])/n[g] − I[¬g](R−mean[¬g])/n[¬g]. Var=Σu²+2Σₗ(1−l/(L+1))Σu[s]u[s−l]. Missing sessions contribute zero; lag distance is preserved.")
        .note(format!("Pointwise CI Δ±1.96 SE: {}. Two-sided normal p; BY = min(1,H₁₁₇ × BH), m={OR_FAMILY}, unavailable p=1.",i.interval.map(|(l,h)|format!("{l:+.2}…{h:+.2} bp")).unwrap_or_else(||"unavailable".into())))
        .caveat("Asymptotic, five-session bandwidth; BY does not repair invalid marginal p. Pointwise intervals are not simultaneous. Repeated symbol/date exploration is outside this family; provider corporate-action limits remain.");
    let card = if let Some(reason) = &i.unavailable {
        card.note(format!("Inference unavailable: {reason}; n ≥ {MIN_N} in EACH group required."))
    } else {
        card
    };
    match i.estimate {
        Some(v) => card.value(bps(Some(v))),
        None => card.unavailable(format!("no group/complement contrast (n={} / {})", i.n, i.complement_n)),
    }
}
fn mean_card(r: &OrLens, c: &OrCell, g: usize) -> MathCard {
    let (label, n, v) = match g {
        0 => (c.state.label(), c.selected.len(), c.mean),
        1 => ("Complement", c.complement.len(), c.complement_mean),
        _ => ("All states", c.selected.len() + c.complement.len(), c.all_mean),
    };
    let card=base(r,c,&format!("{label} MEAN"),"mean = ΣR / n; R = 10⁴(C[t+h]/C[t] − 1)")
        .n(n,"completed session outcomes").input("sum",v.map(|v|format!("{:.4} bp",v*n as f64)).unwrap_or_else(||"—".into()))
        .note("Descriptive mean. All-state context includes the selected group; it is not an independent comparator. Inferential uncertainty belongs to the group–complement contrast.");
    match v {
        Some(v) => card.value(bps(Some(v))),
        None => card.unavailable("no completed outcomes"),
    }
}
pub(in crate::app) fn view(ui: &mut Ui, r: &OrLens, selection: &mut Selection) -> Rect {
    let mut selected_rect = Rect::NOTHING;
    ui.label(RichText::new("OPENING RANGE · conditional lens").font(theme::mono(15.)).color(theme::text()));
    ui.label(
        RichText::new(format!("{} · {} · native 30m · {} … {}", r.symbol, r.provenance.label(), r.start, r.end))
            .small()
            .color(theme::muted()),
    );
    ui.label(
        RichText::new(format!(
            "{} · observed {} · CLOSED THRU {}",
            r.source,
            timestamp(r.observed_at),
            timestamp(r.closed_thru)
        ))
        .small()
        .color(theme::muted()),
    );
    ui.label(format!(
        "Coverage: {}/{} bars · {}/{} sessions · OR30 present {} · TODAY excluded",
        r.bars, r.expected_bars, r.sessions, r.grid_sessions, r.opening_sessions
    ));
    super::super::studio_dist::gap_warning(ui, &r.gaps);
    ui.horizontal_wrapped(|ui| {
        ui.label("Completed horizon");
        for (h, n) in OR_HORIZONS.iter().enumerate() {
            ui.selectable_value(&mut selection.horizon, h, format!("{}m", n * 30));
        }
    });
    ScrollArea::vertical().id_salt("or-lens-scroll").show(ui, |ui| {
        let hs = HeatScale::diverging(r.effect_scale, "Δ bp", |v| format!("{v:.1} bp")).nice();
        hs.key(ui);
        ui.label(
            RichText::new("Origin CLOSE · NY · outline: asymptotic BY q ≤ 0.10 · corner tick: LOW N (<30/group)")
                .small()
                .color(theme::muted()),
        );
        ScrollArea::horizontal().id_salt("or-heat-scroll").show(ui, |ui| {
            let width = ui.available_width().max(900.);
            let (rect, _) = ui.allocate_exact_size(vec2(width, 226.), Sense::hover());
            let w = (width - 72.) / 13.;
            for k in 0..13 {
                ui.painter().text(
                    pos2(rect.left() + 72. + (k as f32 + 0.5) * w, rect.top() + 12.),
                    Align2::CENTER_CENTER,
                    clock(600 + k as u32 * 30),
                    theme::mono(10.),
                    theme::muted(),
                );
            }
            for (g, state) in OrState::ALL.iter().enumerate() {
                ui.painter().text(
                    pos2(rect.left(), rect.top() + 55. + g as f32 * 66.),
                    Align2::LEFT_CENTER,
                    state.label(),
                    theme::mono(12.),
                    theme::series(g),
                );
                for k in 0..13 {
                    let c = r.cell(g, k, selection.horizon);
                    let rc = Rect::from_min_size(
                        pos2(rect.left() + 72. + k as f32 * w, rect.top() + 28. + g as f32 * 66.),
                        vec2(w - 3., 60.),
                    );
                    let v = c.inference.estimate;
                    let low = c.selected.len() < MIN_N || c.complement.len() < MIN_N;
                    ui.painter().rect_filled(rc, 3., theme::control());
                    let heat = Rect::from_min_size(rc.min, vec2(rc.width(), 22.));
                    ui.painter().rect_filled(heat, 2., hs.fill_gated(v, !low));
                    if c.inference.p.is_some() && c.q <= 0.10 {
                        ui.painter().rect_stroke(rc, 3., Stroke::new(2., theme::text()), StrokeKind::Inside);
                    }
                    if g == selection.state && k == selection.clock {
                        ui.painter().rect_stroke(
                            rc.expand(1.),
                            3.,
                            Stroke::new(1., theme::series(g)),
                            StrokeKind::Outside,
                        );
                    }
                    if low {
                        ui.painter().line_segment(
                            [rc.left_top() + vec2(3., 7.), rc.left_top() + vec2(7., 3.)],
                            Stroke::new(2., theme::caution()),
                        );
                    }
                    ui.painter().text(
                        rc.center_top() + vec2(0., 34.),
                        Align2::CENTER_CENTER,
                        v.map(|v| format!("{v:+.2}")).unwrap_or_else(|| "—".into()),
                        theme::mono(10.),
                        v.map(theme::sign).unwrap_or_else(theme::muted),
                    );
                    ui.painter().text(
                        rc.center_bottom() - vec2(0., 8.),
                        Align2::CENTER_CENTER,
                        format!("{}/{}", c.selected.len(), c.complement.len()),
                        theme::mono(9.),
                        theme::muted(),
                    );
                    if g == selection.state && k == selection.clock {
                        selected_rect = rc;
                    }
                    let response = ui
                        .interact(rc, ui.id().with(("or-cell", g, k)), Sense::click())
                        .math_card(|| effect_card(r, c));
                    if response.clicked() {
                        selection.state = g;
                        selection.clock = k;
                    }
                }
            }
        });
        let c = r.cell(selection.state, selection.clock, selection.horizon);
        ui.label(
            RichText::new(format!(
                "{} · {} close → +{}m · same-clock complement",
                c.state.label(),
                clock(c.minute),
                c.horizon * 30
            ))
            .font(theme::mono(13.))
            .color(theme::series(selection.state)),
        );
        let columns = (ui.available_width() / 165.).floor().max(1.) as usize;
        egui::Grid::new("or-statistics").num_columns(columns).show(ui, |ui| {
            for index in 0..7 {
                ui.vertical(|ui| {
                    ui.set_width(152.);
                    if index < 3 {
                        let (label, v) = match index {
                            0 => ("Group mean", c.mean),
                            1 => ("Complement mean", c.complement_mean),
                            _ => ("All-state context", c.all_mean),
                        };
                        widgets::stat_tile(ui, label, &bps(v), v.map(theme::sign).unwrap_or_else(theme::muted), || {
                            mean_card(r, c, index)
                        });
                    } else if index == 3 {
                        widgets::stat_tile(
                            ui,
                            "Δ group − complement",
                            &bps(c.inference.estimate),
                            c.inference.estimate.map(theme::sign).unwrap_or_else(theme::muted),
                            || effect_card(r, c),
                        );
                    } else {
                        let (label, v, formula) = match index {
                            4 => (
                                "HAC SE (bp)",
                                c.inference.se,
                                "SE = sqrt(Bartlett HAC variance); five-session bandwidth",
                            ),
                            5 => ("Normal p", c.inference.p, "p = 2 Φ(−|Δ / SE|); asymptotic two-sided normal test"),
                            _ => (
                                "BY q · m=117",
                                c.inference.p.map(|_| c.q),
                                "q = min(1, H₁₁₇ × BH(p)); all 117 cells, unavailable p=1",
                            ),
                        };
                        widgets::stat_tile(
                            ui,
                            label,
                            &v.map(|v| format!("{v:.4}")).unwrap_or_else(|| "—".into()),
                            theme::text(),
                            || {
                                let mut card = effect_card(r, c);
                                card.title = label.into();
                                card.formula = formula.into();
                                if let Some(reason) = &c.inference.unavailable {
                                    card.unavailable(reason)
                                } else {
                                    card.value(format!("{:.5}", v.unwrap()))
                                }
                            },
                        );
                    }
                });
                if (index + 1) % columns == 0 {
                    ui.end_row();
                }
            }
        });
        if let Some(reason) = &c.inference.unavailable {
            ui.label(
                RichText::new(format!("Inference unavailable · {reason}. Distributions are descriptive."))
                    .small()
                    .color(theme::caution()),
            );
        }
        whisker(ui, r, c);
        histogram(ui, r, c, selection.state);
        ui.label(
            RichText::new(
                "Asymptotic HAC · pointwise 95% CI · fixed 117-cell BY family across all horizons · no recommendations",
            )
            .small()
            .color(theme::muted()),
        );
    });
    selected_rect
}
fn whisker(ui: &mut Ui, r: &OrLens, c: &OrCell) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width().max(100.), 82.), Sense::hover());
    let plot = rect.shrink2(vec2(35., 24.));
    let x = |v: f64| plot.center().x + (v / c.whisker_scale) as f32 * plot.width() * 0.5;
    let y = plot.center().y;
    ui.painter().line_segment([pos2(x(0.), plot.top()), pos2(x(0.), plot.bottom())], Stroke::new(1., theme::border()));
    if let Some((lo, hi)) = c.inference.interval {
        ui.painter().line_segment([pos2(x(lo), y), pos2(x(hi), y)], Stroke::new(2., theme::text()));
        for v in [lo, hi] {
            ui.painter().line_segment([pos2(x(v), y - 5.), pos2(x(v), y + 5.)], Stroke::new(1., theme::text()));
        }
    }
    if let Some(v) = c.inference.estimate {
        ui.painter().circle_filled(pos2(x(v), y), 4., theme::sign(v));
    }
    for v in [-c.whisker_scale, 0., c.whisker_scale] {
        ui.painter().text(
            pos2(x(v), plot.bottom() + 4.),
            Align2::CENTER_TOP,
            format!("{v:+.1}"),
            theme::mono(9.),
            theme::sign(v),
        );
    }
    ui.painter().text(
        rect.center_top(),
        Align2::CENTER_TOP,
        "Δ bp · pointwise 95% HAC interval",
        theme::mono(10.),
        theme::muted(),
    );
    response.math_card(|| effect_card(r, c));
}
fn histogram(ui: &mut Ui, r: &OrLens, c: &OrCell, state: usize) {
    ui.label("NORMALIZED DISTRIBUTIONS · shared return edges (bp) · each group sums to 1 when nonempty");
    let h = &c.histogram;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().max(100.), 204.), Sense::hover());
    let plot = Rect::from_min_max(rect.min + vec2(35., 20.), rect.max - vec2(10., 25.));
    let w = plot.width() / h.mass[0].len() as f32;
    let colors = [theme::series(state), theme::series(3)];
    for (g, ink) in colors.iter().enumerate() {
        let points: Vec<_> = h.mass[g]
            .iter()
            .enumerate()
            .flat_map(|(i, p)| {
                let y = plot.bottom() - (*p / h.max_mass) as f32 * plot.height();
                [pos2(plot.left() + i as f32 * w, y), pos2(plot.left() + (i + 1) as f32 * w, y)]
            })
            .collect();
        let n = if g == 0 { c.selected.len() } else { c.complement.len() };
        if n > 0 {
            ui.painter().add(egui::Shape::line(points, Stroke::new(2., *ink)));
        }
    }
    for i in 0..h.mass[0].len() {
        ui.interact(
            Rect::from_min_size(pos2(plot.left() + i as f32 * w, plot.top()), vec2(w, plot.height())),
            ui.id().with(("or-bin", i)),
            Sense::hover(),
        )
        .math_card(|| histogram_card(r, c, i));
    }
    ui.painter().text(
        plot.left_top() - vec2(2., 4.),
        Align2::LEFT_BOTTOM,
        format!("mass 0 … {:.2}", h.max_mass),
        theme::mono(9.),
        theme::muted(),
    );
    for (x, v) in [
        (plot.left(), h.edges[0]),
        (plot.center().x, (h.edges[0] + h.edges[h.edges.len() - 1]) / 2.),
        (plot.right(), h.edges[h.edges.len() - 1]),
    ] {
        ui.painter().text(
            pos2(x, plot.bottom() + 5.),
            Align2::CENTER_TOP,
            format!("{v:+.1}"),
            theme::mono(9.),
            theme::sign(v),
        );
    }
    ui.horizontal_wrapped(|ui| {
        ui.colored_label(colors[0], format!("{} · n={}", c.state.label(), c.selected.len()))
            .math_card(|| mean_card(r, c, 0));
        ui.colored_label(colors[1], format!("Complement · n={}", c.complement.len())).math_card(|| mean_card(r, c, 1));
    });
}

pub(in crate::app) fn histogram_card(r: &OrLens, c: &OrCell, bin: usize) -> MathCard {
    let h = &c.histogram;
    let mass = |g: usize, n: usize| if n > 0 { format!("{:.3}", h.mass[g][bin]) } else { "—".into() };
    let card = base(r, c, "HISTOGRAM PROBABILITY MASS", "mass[group,bin] = count[group,bin] / n[group]; shared equal-width edges")
        .input("edges (bp)", format!("{:+.3} … {:+.3}", h.edges[bin], h.edges[bin+1]))
        .input("bin counts / total n", format!("{} / {}; {} / {}", h.counts[0][bin], c.selected.len(), h.counts[1][bin], c.complement.len()))
        .note("Descriptive empirical probability mass; no binwise confidence interval or test. Empty groups have no distribution.");
    if c.selected.is_empty() && c.complement.is_empty() {
        card.unavailable("no completed outcomes in either group")
    } else {
        card.value(format!("{} / {}", mass(0, c.selected.len()), mass(1, c.complement.len())))
    }
}
