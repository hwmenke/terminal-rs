//! Relate controls, the Relate panel and the fair-value MathCards (Studio slice 4).
//! Kept out of `report_view`, which the CORR paint gate measures.
use super::*;
use crate::widgets::{MathCard, MathCardExt};
use tr_data::studio::{BetaMode, MatrixMode, Transform};
use tr_data::studio_corr::{FairValueFit, FairValuePoint, RelateReport, RelateSpec};

/// Target-space unit label for column headers and the overlay caption.
pub(crate) fn unit_label(t: Transform) -> &'static str {
    match t {
        Transform::Level => "level, native units",
        Transform::LogLevel => "ln level",
        Transform::Change { .. } => "Δ native units",
        Transform::LogReturn { .. } => "log-return",
    }
}

fn chip(ui: &mut Ui, on: bool, text: &str) -> bool {
    ui.selectable_label(on, RichText::new(text).font(theme::ui_font(11.0))).clicked()
}

/// Transform / β / bands / matrix chips. Returns true when the inputs changed (request a recompute).
pub(super) fn controls(ui: &mut Ui, pane: &mut CorrPane) -> bool {
    let Some(spec) = pane.relate.as_mut() else { return false };
    let mut changed = false;
    let mut horizon = pane.horizon;
    let hh = if spec.transform.is_level() { horizon } else { spec.transform.horizon() };
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Transform").font(theme::ui_font(11.0)).color(theme::muted()));
        for (label, next) in [
            ("Level", Transform::Level),
            ("Δ", Transform::Change { h: hh }),
            ("log-ret", Transform::LogReturn { h: hh }),
            ("log-level", Transform::LogLevel),
        ] {
            let on = std::mem::discriminant(&spec.transform) == std::mem::discriminant(&next);
            if chip(ui, on, label) && !on {
                spec.transform = next;
                changed = true;
            }
        }
        ui.label(RichText::new("h").font(theme::ui_font(11.0)).color(theme::muted()));
        ui.add_enabled_ui(!spec.transform.is_level(), |ui| {
            for h in [1usize, 5, 20] {
                let on = !spec.transform.is_level() && spec.transform.horizon() == h;
                if chip(ui, on, &h.to_string()) && !on {
                    spec.transform = match spec.transform {
                        Transform::Change { .. } => Transform::Change { h },
                        _ => Transform::LogReturn { h },
                    };
                    horizon = h;
                    changed = true;
                }
            }
        });
        ui.separator();
        ui.label(RichText::new("β").font(theme::ui_font(11.0)).color(theme::muted()));
        for (label, mode) in [("Rolling", BetaMode::Rolling), ("Expanding", BetaMode::Expanding)] {
            if chip(ui, spec.beta_mode == mode, label) && spec.beta_mode != mode {
                spec.beta_mode = mode;
                changed = true;
            }
        }
        ui.separator();
        if chip(ui, spec.bands, "Bands") {
            spec.bands = !spec.bands;
            // Bands are only an overlay toggle; the report is unchanged.
        }
        ui.separator();
        ui.label(RichText::new("Matrix").font(theme::ui_font(11.0)).color(theme::muted()));
        for (label, mode) in [("Pairwise", MatrixMode::Pairwise), ("Common panel", MatrixMode::Common)] {
            if chip(ui, spec.matrix == mode, label) && spec.matrix != mode {
                spec.matrix = mode;
                changed = true;
            }
        }
    });
    pane.horizon = horizon;
    changed
}

fn clock(r: &CorrReport) -> String {
    r.observed_at.map(stamp).unwrap_or_else(|| "closed input receipt; clock unavailable".into())
}

pub(crate) fn signed(v: f64, digits: usize) -> String {
    let s = format!("{v:+.digits$}");
    s.replacen('-', "\u{2212}", 1)
}

/// Stable ids: `relate.fit.alpha`, `relate.fit.beta`.
pub(crate) fn fit_card(r: &CorrReport, rel: &RelateReport, fit: &FairValueFit, coefficient: usize) -> MathCard {
    let value = [fit.alpha, fit.beta][coefficient];
    let name = ["relate.fit.alpha", "relate.fit.beta"][coefficient];
    let mut card = MathCard::new(
        format!("RELATE {} · {name}", ["ALPHA", "BETA"][coefficient]),
        "y = α + β·x + ε on the first 80% chronological valid pairs; (α,β)=OLS; σ=√(SSE/(n−2)) in target space",
    )
    .n(fit.n, "training pairs")
    .span(stamp(rel.y.ts.first().copied().unwrap_or(fit.train_end)), stamp(fit.train_end))
    .closed_thru(stamp(r.closed_thru))
    .source(&r.source)
    .provenance(r.provenance)
    .input("transform", format!("{} ({})", fit.transform.short(), unit_label(fit.transform)))
    .input("σ (target space)", format!("{:.6e}", fit.sigma))
    .input("mean-CI covariance", fit.covariance.label())
    .input("observation clock", clock(r))
    .input("grid convention", "strict fixed bars: both series exactly h bars apart, no expected-grid or vault gap")
    .note(format!("Frozen fit thru {} · {}", stamp(fit.train_end), r.source))
    .caveat(
        "Descriptive fit on a frozen training prefix. p and q are withheld (each family member enters at p=1). \
         No cheap/rich verdict. Levels may be nonstationary.",
    );
    if fit.transform.is_level() {
        card = card.note("Descriptive: levels may be nonstationary; spurious-regression risk.");
    }
    card.value(signed(value, 6))
}

/// Overlay hover card, id `relate.fair_value`: formula, substituted inputs, residual band,
/// mean CI and prediction SE, receipts and any unavailable reason.
pub(crate) fn fair_value_card(r: &CorrReport, rel: &RelateReport, fit: &FairValueFit, p: &FairValuePoint) -> MathCard {
    let (formula, anchor) = match fit.transform {
        Transform::Level => ("ŷ = α + β·X_t; band = ŷ ± kσ", "—".to_string()),
        Transform::LogLevel => ("ŷ = exp(α + β·ln X_t + kσ)  (conditional median)", "—".to_string()),
        Transform::Change { .. } => ("ŷ = P_{t−h} + α + β·Δ_hX_t + kσ", p.anchor.map_or("—".into(), |a| format!("{a:.4}"))),
        Transform::LogReturn { .. } => {
            ("ŷ = P_{t−h}·exp(α + β·r^X_{t,h} + kσ)", p.anchor.map_or("—".into(), |a| format!("{a:.4}")))
        }
    };
    let mut card = MathCard::new("FAIR VALUE · relate.fair_value", formula)
        .n(fit.n, "training pairs")
        .span(stamp(rel.y.ts.first().copied().unwrap_or(fit.train_end)), stamp(r.as_of))
        .closed_thru(stamp(r.closed_thru))
        .source(&r.source)
        .provenance(r.provenance)
        .input("α / β", format!("{} / {}", signed(fit.alpha, 6), signed(fit.beta, 6)))
        .input("x (target space)", format!("{:.6}", p.x))
        .input("anchor P_{t−h}", anchor)
        .input("σ (target space)", format!("{:.6e}", fit.sigma))
        .input(
            "residual band −2σ −1σ +1σ +2σ",
            p.bands.iter().map(|v| format!("{v:.4}")).collect::<Vec<_>>().join("  "),
        )
        .input("observation clock", clock(r))
        .input(
            "sample",
            if p.in_sample {
                "in-sample descriptive (fitted training prefix)"
            } else if p.embargo {
                "embargo: within h−1 bars of the training prefix, not fitted"
            } else {
                "out of sample (after the embargo)"
            },
        )
        .input("descriptive z=(y−ŷ)/σ", signed(p.z, 2))
        .note(format!("residual band: ±kσ in target space, then inverted · frozen fit thru {}", stamp(fit.train_end)))
        .caveat(
            "Contemporaneous, not a forecast: Δ and log-return fair values use the realised benchmark move to t. \
             Band ≠ mean CI: the residual band is ±kσ only.",
        );
    match tr_data::studio_corr::hover_stats(fit, p) {
        Some(h) => {
            card = match (h.mean_ci, h.mean_ci_withheld) {
                (Some(ci), _) => card.input("mean CI (95%, ŷ ± 1.96·se_m)", format!("{:.4} … {:.4}", ci[0], ci[1])),
                (None, why) => card.input("mean CI", format!("withheld: {}", why.unwrap_or("unavailable"))),
            };
            card = card
                .input("se_m (mean response)", format!("{:.6e}", h.se_mean))
                .input("prediction SE √(σ²+se_m²)", format!("{:.6e}", h.se_prediction))
                .input("prediction band (±1 SE)", format!("{:.4} … {:.4}", h.prediction_band[0], h.prediction_band[1]))
                .input("mean-CI covariance", fit.covariance.label());
        }
        None => {
            card = card.input("mean CI / prediction SE", "unavailable: covariance unavailable for this fit");
        }
    }
    card.value(format!("{:.4}", p.centre))
}

/// Expanding β sparkline, descriptive only. Breaks (None) split the line.
fn expanding_beta(ui: &mut Ui, r: &CorrReport, rel: &RelateReport) {
    let pts: Vec<(i64, f64)> = rel.expanding.iter().filter_map(|(t, b)| b.map(|b| (*t, b))).collect();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width().clamp(120.0, 900.0), 90.0), Sense::hover());
    #[cfg(test)]
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("relate-probe"), rect));
    ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, theme::grid()), egui::StrokeKind::Inside);
    if pts.len() < 2 {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "expanding β unavailable: fewer than three valid pairs",
            theme::ui_font(11.0),
            theme::muted(),
        );
        return;
    }
    let (lo, hi) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    let span = (hi - lo).max(1e-12);
    let n = rel.expanding.len().max(2);
    let mut line: Vec<Pos2> = Vec::new();
    let painter = ui.painter();
    for (i, (_, b)) in rel.expanding.iter().enumerate() {
        match b {
            Some(b) => {
                let x = rect.left() + rect.width() * i as f32 / (n - 1) as f32;
                let y = rect.bottom() - 4.0 - (rect.height() - 8.0) * ((b - lo) / span) as f32;
                line.push(Pos2::new(x, y));
            }
            None => {
                if line.len() > 1 {
                    painter.add(egui::Shape::line(std::mem::take(&mut line), Stroke::new(1.5, theme::series(0))));
                }
                line.clear();
            }
        }
    }
    if line.len() > 1 {
        painter.add(egui::Shape::line(line, Stroke::new(1.5, theme::series(0))));
    }
    painter.text(rect.left_top() + egui::vec2(4.0, 2.0), egui::Align2::LEFT_TOP, signed(hi, 3), theme::mono(10.0), theme::muted());
    painter.text(rect.left_bottom() + egui::vec2(4.0, -2.0), egui::Align2::LEFT_BOTTOM, signed(lo, 3), theme::mono(10.0), theme::muted());
    response.math_card(|| {
        let last = pts.last().unwrap();
        MathCard::new(
            "RELATE EXPANDING BETA · relate.beta.expanding",
            "β_t = Cxy/Cxx by one-pass Welford co-moments over valid pairs up to t; a break resets",
        )
        .n(pts.len(), "latest endpoints")
        .span(stamp(pts[0].0), stamp(last.0))
        .closed_thru(stamp(r.closed_thru))
        .source(&r.source)
        .provenance(r.provenance)
        .input("convention", "strict fixed-bar transforms")
        .input("observation clock", clock(r))
        .caveat("Descriptive only: no uncertainty is estimated. Endpoints reuse all earlier data, so they are not independent.")
        .value(signed(last.1, 4))
    });
}

/// Fit tile, expanding β, common-panel matrix and the source receipts.
pub(super) fn panel(ui: &mut Ui, id: PaneId, r: &CorrReport, rel: &RelateReport) {
    ui.push_id(("relate-panel", id), |ui| {
        heading(ui, "RELATE · TRANSFORM + FROZEN-FIT FAIR VALUE");
        let spec: &RelateSpec = &rel.spec;
        ui.label(
            RichText::new(format!(
                "{} · {} · strict fixed-bar grid (both series exactly h bars apart; legacy CORR uses common-timestamp adjacency)",
                spec.transform.short(),
                unit_label(spec.transform)
            ))
            .font(theme::ui_font(11.0))
            .color(theme::muted()),
        );
        ui.label(
            RichText::new(format!(
                "asset: {} · benchmark: {} · CLOSED THRU {} · benchmark gaps {}",
                rel.sources[0],
                rel.sources[1],
                stamp(r.closed_thru),
                rel.benchmark_gaps.len()
            ))
            .font(theme::mono(10.0))
            .color(theme::muted()),
        );
        if rel.sources[0] != rel.sources[1] {
            ui.label(
                RichText::new("Cross-source: asset and benchmark come from different sources; timestamps are matched, values are not reconciled.")
                    .font(theme::ui_font(11.0))
                    .color(theme::muted()),
            );
        }
        match &rel.fit {
            Some(fit) => {
                let label = |ui: &mut Ui, name: &str, v: f64, c: usize| {
                    coloured_label(
                        ui,
                        [
                            (format!("{name} "), theme::text()),
                            (signed(v, 6), theme::sign(v)),
                            (format!("  [{}]", unit_label(fit.transform)), theme::muted()),
                        ],
                    )
                    .math_card(|| fit_card(r, rel, fit, c));
                };
                ui.horizontal_wrapped(|ui| {
                    label(ui, "α", fit.alpha, 0);
                    label(ui, "β", fit.beta, 1);
                    ui.label(
                        RichText::new(format!(
                            "σ {:.4e} · train n={} thru {} · in-sample descriptive",
                            fit.sigma,
                            fit.n,
                            stamp(fit.train_end)
                        ))
                        .font(theme::mono(10.0))
                        .color(theme::muted()),
                    );
                });
                if spec.transform.is_level() {
                    ui.label(
                        RichText::new("Descriptive: levels may be nonstationary. p and q withheld.")
                            .font(theme::ui_font(11.0))
                            .color(theme::muted()),
                    );
                }
            }
            None => {
                ui.label(
                    RichText::new(format!(
                        "Fair value unavailable: {}",
                        rel.unavailable.as_deref().unwrap_or("no reason reported")
                    ))
                    .font(theme::ui_font(11.0))
                    .color(theme::muted()),
                );
            }
        }
        if spec.beta_mode == BetaMode::Expanding {
            heading(ui, "EXPANDING β · descriptive");
            expanding_beta(ui, r, rel);
        }
        if let Some(m) = &rel.matrix_common {
            heading(ui, "CORRELATION · COMMON PANEL");
            let ns: Vec<usize> = m
                .cells
                .iter()
                .enumerate()
                .flat_map(|(i, row)| row.iter().enumerate().filter(move |(j, _)| *j != i).map(|(_, c)| c.n))
                .collect();
            let text = match (ns.iter().min(), ns.iter().max()) {
                (Some(a), Some(b)) if a == b => format!("same n={a} all cells (one complete-case set, strict endpoints)"),
                (Some(a), Some(b)) => format!("n={a}…{b} (a row is unavailable)"),
                _ => "no admitted pair".into(),
            };
            ui.label(RichText::new(text).font(theme::mono(10.0)).color(theme::muted()));
            ui.push_id("common-matrix", |ui| matrix_impl(ui, m, true));
        }
    });
}
