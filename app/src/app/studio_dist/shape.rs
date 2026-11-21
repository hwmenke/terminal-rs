//! Paint only: every distribution and rolling quantile is prepared by the worker.
use super::{Plot, Report, pct};
use crate::{
    app::studio_frame,
    theme,
    widgets::{HeatScale, MathCardExt},
};
use egui::{Rect, Stroke, Ui, pos2};
use tr_data::studio_shape::{Intervals, Shape};
fn signed_pct(x: f64) -> String {
    format!("{:+.2}%", x * 100.).replace('-', "−")
}
fn card(r: &Report, s: &Shape, id: &str, formula: &str) -> crate::widgets::MathCard {
    studio_frame::card(r,s.values.len(),id,formula).input("horizon / sampling",format!("{} bars / {}",s.horizon,if s.overlap{"overlapping"}else{"non-overlapping"})).caveat("Descriptive history, not a forecast. Stationary bootstrap assumes local stationarity; vault seams reset blocks. No multiple-testing claims.")
}
fn tail_card(r: &Report, s: &Shape, i: usize, ci: Option<(f64, f64)>) -> crate::widgets::MathCard {
    let c=card(r,s,"dist.shape.tail","Loss VaR=−Q(α), ES=−mean(x≤Q(α)); gain VaR=Q(1−α), ES=mean(x≥Q(1−α)); α=.05/.01")
        .value(signed_pct(s.tails[i]))
        .input("90% percentile stationary-bootstrap CI",ci.map(|(a,b)|format!("{} … {}",signed_pct(a),signed_pct(b))).unwrap_or_else(||"unavailable / pending".into()))
        .input("level / side / estimator",format!("{} / {} / {}",if i%4<2{95}else{99},if i<4{"loss"}else{"gain"},if i%2==0{"VaR"}else{"ES"}))
        .note("Acerbi–Tasche (2002); 500 deterministic draws, mean geometric block length ceil(n^(1/3)). Inclusive empirical tails; pointwise intervals need n≥30, 99% needs n≥100. Intervals need not contain the estimate.");
    if ci.is_none() {
        c.note("Interval unavailable below sample gate, in descriptive mode, while pending, or after worker failure")
    } else {
        c
    }
}
fn threshold_card(
    r: &Report,
    s: &Shape,
    t: &tr_data::studio_shape::Threshold,
    block: Option<(f64, f64)>,
    length: Option<usize>,
) -> crate::widgets::MathCard {
    card(
        r,
        s,
        "dist.shape.threshold",
        "P(|r|≥X)=k/n; 95% percentile stationary block bootstrap; Normal=P(Z≥(X−μ)/s)+P(Z≤(−X−μ)/s)",
    )
    .value(format!("{} / {}", t.k, s.values.len()))
    .input("X", pct(t.level))
    .input(
        "block-bootstrap 95% (Evidence)",
        block.map(|(a, b)| format!("{:.4} … {:.4}", a, b)).unwrap_or_else(|| "unavailable / pending / Descriptive".into()),
    )
    .input(
        "mean block length",
        length.map(|l| format!("{l} = max(ceil(n^(1/3)), h if overlapping)")).unwrap_or_else(|| "pending".into()),
    )
    .note("500 deterministic draws; seams reset blocks; overlapping h-sums keep at least h neighbours together. Needs n≥30; otherwise unavailable (no Wilson substitution).")
    .note(format!(
        "About — iid Wilson comparison only: {:.4} … {:.4}. Wilson (1927) assumes independent events; overlapping observations violate that, so it is not the Evidence interval.",
        t.interval.0, t.interval.1
    ))
    .caveat("Normal is a fitted descriptive comparator, not a forecast.")
}
#[cfg(test)]
pub(in crate::app) fn view(ui: &mut Ui, r: &Report, s: &Shape, intervals: Option<&Intervals>) -> Rect {
    view_mode(ui, r, s, intervals, true)
}
pub(in crate::app) fn view_mode(
    ui: &mut Ui,
    r: &Report,
    s: &Shape,
    intervals: Option<&Intervals>,
    evidence: bool,
) -> Rect {
    let intervals = if evidence { intervals } else { None };
    studio_frame::receipt(
        ui,
        r,
        "DISTRIBUTION SHAPE",
        "Descriptive · log returns · gap-safe horizons · pointwise 90% stationary-bootstrap intervals",
    );
    ui.label(format!(
        "n={} · h={} · {} · {}",
        s.values.len(),
        s.horizon,
        if s.overlap { "OVERLAPPING" } else { "NON-OVERLAPPING" },
        if s.quantised { "TICK-QUANTISED" } else { "MAD-σ" }
    ));
    ui.colored_label(theme::sign(s.mean), format!("Mean {}", signed_pct(s.mean)))
        .math_card(|| card(r, s, "dist.shape.mean", "μ=Σx/n").value(signed_pct(s.mean)));
    let probe = ui
        .label(format!(
            "Sample sd {} · robust σ {} · sd/robust {}",
            pct(s.sd),
            pct(s.robust),
            if s.robust > 0. { format!("{:.3}", s.sd / s.robust) } else { "— (zero robust scale)".into() }
        ))
        .math_card(|| {
            card(
                r,
                s,
                "dist.shape.scale",
                "mean=Σx/n; sd=√(Σ(x−mean)²/(n−1)); MAD-σ=1.4826 median|x−median|; tick scale=√(π/2) mean|x−median|",
            )
            .value(format!("sd={} robust={}", s.sd, s.robust))
            .input("zero-share flag", "strictly more than 30% exactly-zero returns")
            .note("Hampel (1974). Scale ratio unavailable when robust scale is zero.")
        })
        .rect;
    ui.colored_label(theme::sign(s.now), format!("NOW {} · percentile {:.2}%", signed_pct(s.now), 100. * s.percentile))
        .math_card(|| {
            card(r, s, "dist.shape.now", "percentile = count(x ≤ latest horizon return)/n")
                .value(format!("{} / {:.4}", s.now, s.percentile))
        });
    ui.label("HISTOGRAM · KDE / Normal / Student-t · log-return %; density");
    let lo = s.curves[0][0];
    let hi = s.curves.last().unwrap()[0].max(lo + 1e-12);
    let width = s.bins[0].1 - s.bins[0].0;
    let ymax = s
        .curves
        .iter()
        .flat_map(|x| x[1..].iter())
        .copied()
        .chain(s.bins.iter().map(|b| b.2 as f64 / (s.values.len() as f64 * width)))
        .fold(0., f64::max)
        .max(1.);
    let p = Plot::new(ui, 210., [lo, hi], [0., ymax * 1.05], pct, |v| format!("{v:.1}"));
    for b in &s.bins {
        let a = p.pos(b.0, 0.);
        let z = p.pos(b.1, b.2 as f64 / (s.values.len() as f64 * width));
        p.painter.rect_filled(
            Rect::from_min_max(pos2(a.x, z.y), pos2(z.x, a.y)),
            0.,
            theme::series(0).gamma_multiply(0.35),
        );
    }
    for k in 1..4 {
        if (k == 1 && s.bandwidth.is_none()) || (k == 2 && s.sd == 0.) || (k == 3 && s.student_df.is_none()) {
            continue;
        }
        p.line(s.curves.iter().map(|x| (x[0], x[k])), theme::series([0, 4, 5][k - 1]));
    }
    let pin = p.pos(s.now, 0.);
    p.painter.line_segment([pin, pos2(pin.x, p.rect.top())], Stroke::new(1., theme::text()));
    let hover = p.hover_x();
    let hovered_bin = hover.and_then(|x| s.bins.iter().find(|b| x >= b.0 && x <= b.1));
    p.response.math_card(||card(r,s,"dist.shape.histogram","FD width=2 IQR n^(−1/3), bins clamped 12…80; Gaussian KDE bandwidth=0.9 min(sd,IQR/1.34)n^(−1/5)").value(format!("{} bins",s.bins.len())).input("Silverman (1986) bandwidth",format!("{:?}",s.bandwidth)).input("hovered bin edges / count / mass",hovered_bin.map(|b|format!("{} … {}; {} / {} = {:.4}",signed_pct(b.0),signed_pct(b.1),b.2,s.values.len(),b.2 as f64/s.values.len() as f64)).unwrap_or_else(||"hover an observed bin".into())).note("Freedman–Diaconis (1981). KDE uses 128 grid points. Normal uses sample sd; Student-t moment fit df=4+6/excess and population variance, withheld when excess≤0. Flat inputs have no density fits."));
    if s.bandwidth.is_none() {
        ui.colored_label(theme::muted(), "KDE unavailable: zero Silverman bandwidth");
    }
    if s.student_df.is_none() {
        ui.colored_label(theme::muted(), "Student-t unavailable: zero variance or nonpositive excess kurtosis");
    }
    ui.horizontal_wrapped(|ui| {
        for (k, name) in ["KDE", "Normal", "Student-t"].iter().enumerate() {
            ui.colored_label(theme::series([0, 4, 5][k]), *name);
        }
    });
    ui.horizontal_wrapped(|ui| {
        for (i, q) in s.ladder.iter().enumerate() {
            ui.colored_label(theme::sign(*q), format!("p{} {}", [1, 5, 10, 25, 50, 75, 90, 95, 99][i], signed_pct(*q)))
                .math_card(|| {
                    card(r, s, "dist.shape.ladder", "Type-7 quantile: x[j]+f(x[j+1]−x[j]), j+f=(n−1)p")
                        .value(pct(*q))
                        .note("Quantiles describe observations, not confidence limits.")
                });
        }
    });
    ui.label("BOTH TAILS · 95 / 99 · VaR and ES · log-return %");
    studio_frame::low_n(ui, s.values.len(), 30);
    if evidence && intervals.is_none() {
        ui.label("90% intervals unavailable or pending · 500 stationary-bootstrap draws");
    }
    for (i, v) in s.tails.iter().enumerate() {
        let name = format!(
            "{} {} {}",
            if i < 4 { "Loss" } else { "Gain" },
            if i % 4 < 2 { "95" } else { "99" },
            if i % 2 == 0 { "VaR" } else { "ES" }
        );
        let ci = intervals.and_then(|b| b.tails[i]);
        ui.horizontal_wrapped(|ui| {
            ui.label(&name);
            ui.colored_label(theme::sign(*v), signed_pct(*v)).math_card(|| tail_card(r, s, i, ci));
            if let Some((a, b)) = ci {
                ui.label("90% [");
                ui.colored_label(theme::sign(a), signed_pct(a)).math_card(|| tail_card(r, s, i, ci));
                ui.colored_label(theme::sign(b), signed_pct(b)).math_card(|| tail_card(r, s, i, ci));
                ui.label("]");
                let (rect, response) = ui.allocate_exact_size(egui::vec2(100., 16.), egui::Sense::hover());
                response.math_card(|| tail_card(r, s, i, ci));
                let low = a.min(*v);
                let high = b.max(*v);
                let span = (high - low).max(1e-12);
                let point =
                    |value: f64| pos2(rect.left() + ((value - low) / span) as f32 * rect.width(), rect.center().y);
                ui.painter().line_segment([point(a), point(b)], Stroke::new(1., theme::muted()));
                for value in [a, b] {
                    let at = point(value);
                    ui.painter().line_segment(
                        [at - egui::vec2(0., 4.), at + egui::vec2(0., 4.)],
                        Stroke::new(1., theme::muted()),
                    );
                }
                ui.painter().circle_filled(point(*v), 3., theme::series(0));
            } else {
                ui.label(if i % 4 >= 2 && s.values.len() < 100 {
                    "CI — needs n≥100"
                } else if s.values.len() < 30 {
                    "CI — needs n≥30"
                } else {
                    "CI — unavailable / pending / hidden"
                });
            }
        });
    }
    ui.label("ABSOLUTE MOVE THRESHOLDS · empirical vs fitted Normal");
    for (k, t) in s.thresholds.iter().enumerate() {
        let block = intervals.and_then(|b| b.thresholds.get(k).copied().flatten());
        let interval = if !evidence {
            String::new()
        } else if let Some((a, b)) = block {
            format!(" [block 95% {:.2}…{:.2}%]", 100. * a, 100. * b)
        } else if s.values.len() < tr_data::studio_shape::THRESHOLD_MIN_N {
            format!(" [block CI — needs n≥{}]", tr_data::studio_shape::THRESHOLD_MIN_N)
        } else {
            " [block CI — pending / unavailable]".into()
        };
        let normal = t.normal.map(|v| format!("{:.2}%", 100. * v)).unwrap_or_else(|| "— zero variance".into());
        ui.label(format!("|r| ≥ {} · {:.2}%{interval} · Normal {normal}", pct(t.level), 100. * t.probability))
            .math_card(|| threshold_card(r, s, t, block, intervals.map(|b| b.threshold_block)));
    }
    ui.label("YEAR STRIP · mean / horizon; annualised sample sd");
    ui.horizontal_wrapped(|ui| {for y in &s.years {
        ui.label(format!("{} n={} vol {}",y.year,y.n,y.annual_vol.map(pct).unwrap_or_else(||"—".into()))).math_card(||card(r,s,"dist.shape.year","mean=Σx/n_year; realised vol=sample_sd(x_year)√(nominal periods/year ÷ h)").n(y.n,"year horizon returns").value(format!("mean {} vol {:?}",y.mean,y.annual_vol)).input("nominal annual periods",format!("{}",r.annual_periods)).note("UTC ending-bar year; annualisation is approximate with irregular bars/gaps. Vol unavailable for n<2."));
        ui.colored_label(theme::sign(y.mean),format!("mean {:+.3}%",100.*y.mean).replace('-',"−")).math_card(||card(r,s,"dist.shape.year.mean","μ_year=Σx_year/n_year").n(y.n,"year horizon returns").value(signed_pct(y.mean)));
    }});
    ui.label("ROLLING QUANTILE RIVER · preceding 250 observations, current excluded · UTC time");
    let heat = HeatScale::sequential(0.90, "central probability mass", |v| format!("{:.0}%", v * 100.));
    heat.key(ui);
    if s.river.is_empty() {
        ui.label("Rolling bands unavailable: needs 251 consecutive horizon observations").math_card(|| {
            card(r, s, "dist.shape.river", "Type-7 q05/q25/q50/q75/q95 of x[t−250…t), excluding t")
                .unavailable("needs 251 consecutive horizon observations")
        });
    } else {
        let time_at = |i: usize| s.timestamps[s.river_indices[i]] as f64;
        let p = Plot::new(
            ui,
            190.,
            [time_at(0), time_at(s.river.len() - 1).max(time_at(0) + 1.)],
            [lo, hi],
            |v| tr_data::stats::bar_date(v as i64),
            pct,
        );
        let stride = s.river.len().div_ceil(p.rect.width().max(1.) as usize).max(1);
        for (i, q) in s.river.iter().enumerate().step_by(stride) {
            for (a, b, mass) in [(0, 4, 0.9), (1, 3, 0.5)] {
                let end = (i + stride).min(s.river.len() - 1);
                if s.river_indices[end] - s.river_indices[i] > stride {
                    continue;
                }
                let a = p.pos(time_at(i), q[a]);
                let b = p.pos(time_at(end), q[b]);
                p.painter.with_clip_rect(p.rect).rect_filled(
                    Rect::from_min_max(pos2(a.x, b.y), pos2(b.x, a.y)),
                    0.,
                    heat.fill(mass),
                );
            }
        }
        let mut start = 0;
        for i in 1..=s.river.len() {
            if i == s.river.len() || s.river_indices[i] != s.river_indices[i - 1] + 1 {
                p.line((start..i).map(|k| (time_at(k), s.river[k][2])), theme::series(0));
                start = i;
            }
        }

        p.response.math_card(||card(r,s,"dist.shape.river","Type-7 q05/q25/q50/q75/q95 of previous 250 observations, excluding current; reset at gaps").value(format!("{} eligible windows",s.river.len())).note("Central 90% and 50% ranges are descriptive, not confidence bands. UTC ending timestamps; windows and connecting segments across gaps are omitted."));
    }
    probe
}
