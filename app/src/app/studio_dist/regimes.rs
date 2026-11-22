//! Shared-scale, precomputed probability masses. No observation scans in paint.
use super::{REGIMES, Report, gap_warning, regime_color, stamp};
use crate::{
    theme,
    widgets::{MathCard, MathCardExt},
};
use egui::{Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use tr_data::studio_regime::{Q_PASS, RIDGE_BINS, RegimeRotation};
fn bp(v: Option<f64>) -> String {
    v.map(|v| format!("{:+.2}", v * 1e4).replace('-', "−")).unwrap_or_else(|| "—".into())
}
fn opt(v: Option<f64>) -> String {
    v.map(|v| format!("{v:.4}")).unwrap_or_else(|| "—".into())
}
fn base(r: &Report, g: usize, id: &str, formula: &str) -> MathCard {
    MathCard::new(id, formula)
        .n(r.regime.ridges[g].n, "classified closed returns")
        .span(stamp(r.start), stamp(r.end))
        .closed_thru(stamp(r.end))
        .provenance(r.provenance)
        .source(&r.source)
        .input("regime / complement n", format!("{} / {}", REGIMES[g], r.regime.ridges[g].complement_n))
}
pub(in crate::app) fn mass_card(r: &Report, g: usize, k: usize) -> MathCard {
    let f = &r.regime;
    let a = &f.ridges[g];
    let step = 2. * f.half_width / RIDGE_BINS as f64;
    let card = base(r, g, "dist.regime.mass", "mass[k] = count[k] / n_g; outside shares retained")
        .input(
            "shared bin edges",
            format!(
                "−W + k·2W/48; W={:.8}; [{:.8}, {:.8}]",
                f.half_width,
                -f.half_width + k as f64 * step,
                -f.half_width + (k + 1) as f64 * step
            ),
        )
        .input("count / n", format!("{} / {}", a.counts[k], a.n));
    if a.n > 0 {
        card.value(format!("{:.4}%", 100. * a.counts[k] as f64 / a.n as f64))
    } else {
        card.unavailable("no classified returns in this regime")
    }
}
pub(in crate::app) fn quantiles_card(r: &Report, g: usize) -> MathCard {
    let card = base(r, g, "dist.regime.quantiles", "Type-7: x[j] + fraction·(x[j+1]−x[j]); j=(n−1)p")
        .note("q05, q25, q50, q75, q95 describe the observed distribution; they are not confidence limits.");
    match r.regime.ridges[g].quantiles {
        Some(q) => card.value(format!("{:?} log return", q)),
        None => card.unavailable("no classified returns"),
    }
}
pub(in crate::app) fn mean_card(r: &Report, g: usize) -> MathCard {
    let a = &r.regime.ridges[g];
    let h = &a.contrast;
    let complement = a.mean.zip(h.estimate).map(|(m, d)| m - d);
    let card=base(r,g,"dist.regime.mean_contrast","Δ=μ_g−μ_c; u_s=±(r_s−μ_side)/n_side; V=Σu²+2Σ_l(1−l/(L+1))Σ_same_run u_s u_(s−l); CI=Δ±1.96√V; p=2Φ(−|Δ/SE|)")
        .input("μ_g / μ_c bp",format!("{} / {}",bp(a.mean),bp(complement)))
        .input("n_g / n_c / bar-grid span",format!("{} / {} / {}",a.n,a.complement_n,h.span))
        .input("method",format!("HAC Bartlett L=20 (effective {})",h.lag))
        .input("SE bp / pointwise 95% CI bp",format!("{} / {}",bp(h.se),h.interval.map(|(a,b)|format!("{} … {}",bp(Some(a)),bp(Some(b)))).unwrap_or_else(||"—".into())))
        .input("p / BY q, m=3 (mean family)",format!("{} / {:.6}",opt(h.p),a.contrast_q))
        .caveat("Newey–West (1987), BY (2001). L=20 is declared because labels use 20 past returns; inference remains bandwidth-sensitive (Andrews 1991). Missing slots retain lag distance; vault joins reset products. Pointwise, asymptotic intervals.");
    let card = if let Some(reason) = &h.unavailable {
        card.note(format!("Inference unavailable: {reason}; BY input p=1"))
    } else {
        card
    };
    match h.estimate {
        Some(v) => card.value(format!("{} bp", bp(Some(v)))),
        None => card.unavailable("no group or complement returns"),
    }
}
pub(in crate::app) fn distance_card(r: &Report, g: usize, rotation: Option<&RegimeRotation>) -> MathCard {
    let a = &r.regime.ridges[g];
    let m = r.regime.classified;
    let card=base(r,g,"dist.regime.distance","D=sup_x |F_g(x)−F_¬g(x)|, right-continuous tie-complete ECDFs; p=(1+#{D_k≥D})/(K+1)")
        .input("method",format!("{} circular label rotations, shifts 40…{}; fixed values",rotation.map_or(0,|x|x.draws),m.saturating_sub(40)))
        .input("p / BY q, m=3 (distance family)",rotation.map(|x|format!("{} / {:.6}",opt(x.p[g]),x.q[g])).unwrap_or_else(||"pending".into()))
        .input("null 95% band",rotation.and_then(|x|x.null_band[g]).map(|(a,b)|format!("{a:.6} … {b:.6}")).unwrap_or_else(||"unavailable".into()))
        .caveat("Smirnov distance; Theiler et al. (1992) time-shift surrogate, Phipson–Smyth (2010) add-one p. Rotation preserves each series' serial dependence, breaks only alignment, assumes stationarity and has one wrap seam; vault gaps add seams. Small p mostly reflects volatility persistence: labels depend on past |returns|. It is not directional edge. No iid KS tables.");
    let reason = rotation.and_then(|x| x.unavailable.as_deref()).or_else(|| {
        if a.n < 30 || a.complement_n < 30 {
            Some("LOW N: distance inference needs 30 per side")
        } else if rotation.is_none() {
            Some("rotation unavailable or pending; see distance status")
        } else {
            None
        }
    });
    let card = if let Some(reason) = reason {
        card.note(format!("Distance p unavailable: {reason}; D is descriptive"))
    } else {
        card
    };
    match &a.distance {
        Some(d) => card.value(format!("D {:.6} at x*={:.8}", d.d, d.at)),
        None => card.unavailable("empty group or complement"),
    }
}
pub(in crate::app) fn up_card(r: &Report, g: usize) -> MathCard {
    let card=base(r,g,"dist.regime.up_share","Wilson 95%: centre=(p̂+z²/2n)/(1+z²/n); half=z√(p̂(1−p̂)/n+z²/4n²)/(1+z²/n); z=1.96")
        .caveat("Wilson (1927) treats signs as independent; serial dependence is not corrected. Zero returns stay in n and do not count as up.");
    match &r.regime.ridges[g].up {
        Some(u) => card
            .value(format!("{:.2}% [{:.2}, {:.2}]", u.share * 100., u.interval.0 * 100., u.interval.1 * 100.))
            .input("k / n", format!("{} / {}", u.k, u.n)),
        None => card.unavailable("no classified returns"),
    }
}
pub(in crate::app) fn tails_card(r: &Report, g: usize) -> MathCard {
    let f = &r.regime;
    let a = &f.ridges[g];
    let card = base(r, g, "dist.regime.tails", "count(r<μ_pool−2s), count(r>μ_pool+2s); s²=Σ(r−μ_pool)²/(n_pool−1)")
        .input("pooled mean / sample sd", format!("{:.8} / {:.8}", f.pooled_mean, f.pooled_sigma))
        .input("beyond shared axis −W / +W", format!("{} / {}", a.outside[0], a.outside[1]))
        .note("s is the pooled sample standard deviation (n−1) of classified returns; strict tail inequalities.");
    match a.beyond {
        Some([lo, hi]) => card.value(format!(
            "{lo} ({:.2}%) / {hi} ({:.2}%)",
            lo as f64 / a.n as f64 * 100.,
            hi as f64 / a.n as f64 * 100.
        )),
        None => card.unavailable("no classified returns"),
    }
}
fn ridge(ui: &mut Ui, r: &Report, g: usize, width: f32, max: f64) {
    let a = &r.regime.ridges[g];
    let w = r.regime.half_width;
    let (rect, _) = ui.allocate_exact_size(vec2(width.max(80.), 92.), Sense::hover());
    let plot = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + 58.));
    let x = |v: f64| plot.left() + ((v / w + 1.) * 0.5).clamp(0., 1.) as f32 * plot.width();
    ui.painter().line_segment([pos2(x(0.), plot.top()), pos2(x(0.), plot.bottom())], Stroke::new(1., theme::muted()));
    for k in 0..RIDGE_BINS {
        let mass = if a.n > 0 { a.counts[k] as f64 / a.n as f64 } else { 0. };
        let bin = Rect::from_min_max(
            pos2(plot.left() + k as f32 * plot.width() / 48., plot.bottom() - (mass / max) as f32 * 50.),
            pos2(plot.left() + (k + 1) as f32 * plot.width() / 48. - 0.5, plot.bottom()),
        );
        ui.painter().rect_filled(bin, 0., regime_color(g));
        let hit = Rect::from_min_max(pos2(bin.left(), plot.top()), pos2(bin.right(), plot.bottom()));
        ui.interact(hit, ui.id().with(("bin", g, k)), Sense::hover()).math_card(|| mass_card(r, g, k));
    }
    if let Some(q) = a.quantiles {
        let y = plot.bottom() + 7.;
        ui.painter().line_segment([pos2(x(q[1]), y), pos2(x(q[3]), y)], Stroke::new(4., regime_color(g)));
        for v in [q[0], q[2], q[4]] {
            ui.painter().line_segment([pos2(x(v), y - 4.), pos2(x(v), y + 4.)], Stroke::new(1., theme::text()));
        }
        ui.interact(
            Rect::from_min_max(pos2(plot.left(), y - 5.), pos2(plot.right(), y + 5.)),
            ui.id().with(("quantiles", g)),
            Sense::hover(),
        )
        .math_card(|| quantiles_card(r, g));
    }
    for side in &a.rug {
        for &v in side {
            let y = plot.bottom() + 18.;
            ui.painter().line_segment([pos2(x(v), y), pos2(x(v), y + 3.)], Stroke::new(1., regime_color(g)));
        }
    }
    for (v, label, align) in [
        (-w, format!("{:.3}%", -w * 100.), egui::Align2::LEFT_BOTTOM),
        (0., "0".into(), egui::Align2::CENTER_BOTTOM),
        (w, format!("+{:.3}%", w * 100.), egui::Align2::RIGHT_BOTTOM),
    ] {
        ui.painter().text(pos2(x(v), rect.bottom()), align, label, theme::mono(10.), theme::muted());
    }
}
fn stats(ui: &mut Ui, r: &Report, g: usize, rotation: Option<&RegimeRotation>, loading: bool) -> Rect {
    let a = &r.regime.ridges[g];
    let h = &a.contrast;
    let low = a.n < 30 || a.complement_n < 30;
    let pass = h.p.is_some() && a.contrast_q <= Q_PASS;
    let (cell, response) = ui.allocate_exact_size(vec2(210., 54.), Sense::hover());
    let ink = if pass { theme::text() } else { theme::muted() };
    let signed_ink = |v| theme::sign(v).gamma_multiply(if pass { 1. } else { 0.65 });
    if pass {
        ui.painter().rect_stroke(cell.shrink(1.), 0., Stroke::new(1., theme::text()), StrokeKind::Inside);
    }
    ui.painter().text(cell.left_top() + vec2(4., 3.), egui::Align2::LEFT_TOP, "Δμ bp", theme::mono(11.), ink);
    ui.painter().text(
        cell.left_top() + vec2(64., 3.),
        egui::Align2::LEFT_TOP,
        bp(h.estimate),
        theme::mono(11.),
        signed_ink(h.estimate.unwrap_or(0.)),
    );
    if low {
        ui.painter().line_segment(
            [cell.right_top() - vec2(7., 0.), cell.right_top() + vec2(0., 7.)],
            Stroke::new(2., theme::amber()),
        );
    }
    if let Some((lo, hi)) = h.interval {
        let extent = lo.abs().max(hi.abs()).max(1e-12);
        let x = |v: f64| cell.center().x + (v / extent) as f32 * 75.;
        let y = cell.top() + 29.;
        ui.painter().line_segment(
            [pos2(cell.center().x, y - 5.), pos2(cell.center().x, y + 5.)],
            Stroke::new(1., theme::muted()),
        );
        ui.painter().line_segment([pos2(x(lo), y), pos2(x(hi), y)], Stroke::new(2., regime_color(g)));
        for v in [lo, hi] {
            ui.painter().line_segment([pos2(x(v), y - 3.), pos2(x(v), y + 3.)], Stroke::new(1., regime_color(g)));
        }
        ui.painter().circle_filled(pos2(x(h.estimate.unwrap()), y), 2.5, regime_color(g));
        ui.painter().text(
            cell.left_bottom() - vec2(-4., 1.),
            egui::Align2::LEFT_BOTTOM,
            bp(Some(lo)),
            theme::mono(10.),
            signed_ink(lo),
        );
        ui.painter().text(
            cell.right_bottom() - vec2(4., 1.),
            egui::Align2::RIGHT_BOTTOM,
            bp(Some(hi)),
            theme::mono(10.),
            signed_ink(hi),
        );
    }
    ui.painter().text(
        cell.center_top() + vec2(65., 3.),
        egui::Align2::CENTER_TOP,
        if low { "LOW N".into() } else { format!("q {:.3}", a.contrast_q) },
        theme::mono(10.),
        ink,
    );
    response.math_card(|| mean_card(r, g));
    let q = rotation.map(|x| x.q[g]);
    let p = rotation.and_then(|x| x.p[g]);
    let d = a.distance.as_ref().map(|d| format!("D {:.3}", d.d)).unwrap_or_else(|| "D —".into());
    let label = if low {
        format!("{d} · descriptive · LOW N")
    } else if p.is_some() {
        format!("{d} · q {:.3}", q.unwrap())
    } else if loading {
        format!("{d} · p …")
    } else {
        format!("{d} · descriptive")
    };
    let pass = p.is_some() && q.unwrap_or(1.) <= Q_PASS;
    let response = ui.label(RichText::new(label).color(if pass { theme::text() } else { theme::muted() }));
    if pass {
        ui.painter().rect_stroke(response.rect.expand(2.), 0., Stroke::new(1., theme::text()), StrokeKind::Inside);
    }
    if low {
        ui.painter().line_segment(
            [response.rect.right_top() - vec2(7., 0.), response.rect.right_top() + vec2(0., 7.)],
            Stroke::new(2., theme::amber()),
        );
    }
    response.math_card(|| distance_card(r, g, rotation));
    ui.label(
        RichText::new(
            a.up.as_ref()
                .map(|u| {
                    format!("P(r>0) {:.1}% [{:.1}, {:.1}]", u.share * 100., u.interval.0 * 100., u.interval.1 * 100.)
                })
                .unwrap_or_else(|| "P(r>0) —".into()),
        )
        .color(theme::muted()),
    )
    .math_card(|| up_card(r, g));
    cell
}
pub(in crate::app) fn view(
    ui: &mut Ui,
    r: &Report,
    rotation: Option<&RegimeRotation>,
    loading: bool,
    sel: &mut usize,
) -> Rect {
    gap_warning(ui, &r.gaps);
    ui.label(
        RichText::new(format!("REGIME FINGERPRINTS · {} log returns by lagged-vol regime", r.timeframe.label()))
            .strong(),
    );
    let parts = crate::widgets::chip_one(&r.symbol, &r.source_receipt);
    crate::widgets::source_chip(ui, "", &parts, || {
        format!("CLOSED THRU {} · {} · {}", stamp(r.end), r.provenance.label(), r.source)
    });
    let f = &r.regime;
    ui.label(format!(
        "classified {} / {} returns · warm-up {} · gap-excluded {}",
        f.classified, f.slots, f.warmup, f.gap_excluded
    ));
    ui.label(
        RichText::new("mean family m=3 (BY) · distance family m=3 (BY) · HAC Bartlett L=20 · corner tick: LOW N (<30)")
            .small(),
    );
    ui.label(
        RichText::new(format!(
            "{} rotations · {}",
            rotation.map_or(499, |r| r.draws),
            r.observed_at
                .map(|t| format!("observed {}", stamp(t)))
                .unwrap_or_else(|| "observation unavailable; final bar withheld".into())
        ))
        .small(),
    );
    if let Some(reason) = &f.unavailable {
        ui.label(RichText::new(reason).color(theme::muted()));
        return Rect::NOTHING;
    }
    if loading {
        ui.label("computing 499 label rotations…");
    }
    if let Some(reason) = rotation.and_then(|x| x.unavailable.as_deref()) {
        ui.label(format!("distance p unavailable: {reason}"));
    }
    let max = f
        .ridges
        .iter()
        .flat_map(|r| r.counts.iter().map(move |n| *n as f64 / r.n.max(1) as f64))
        .fold(0f64, f64::max)
        .max(1e-12);
    let mut probe = Rect::NOTHING;
    for g in 0..3 {
        let a = &f.ridges[g];
        ui.separator();
        ui.horizontal(|ui| {
            ui.colored_label(regime_color(g), "■");
            if ui.selectable_label(*sel == g, REGIMES[g]).clicked() {
                *sel = g;
            }
            ui.label(format!("n={}", a.n)).math_card(|| quantiles_card(r, g));
        });
        let wide = ui.available_width() >= 700.;
        let width = if wide { ui.available_width() * 0.36 } else { ui.available_width() };
        if wide {
            ui.horizontal(|ui| {
                ridge(ui, r, g, width, max);
                ui.vertical(|ui| {
                    let cell = stats(ui, r, g, rotation, loading);
                    if g == *sel {
                        probe = cell;
                    }
                });
            });
        } else {
            ridge(ui, r, g, width, max);
            ui.horizontal_wrapped(|ui| {
                let cell = stats(ui, r, g, rotation, loading);
                if g == *sel {
                    probe = cell;
                }
            });
        }
        let tail = a
            .beyond
            .map(|[lo, hi]| {
                format!(
                    "<−2s: {lo} ({:.1}%) · >+2s: {hi} ({:.1}%)",
                    lo as f64 / a.n.max(1) as f64 * 100.,
                    hi as f64 / a.n.max(1) as f64 * 100.
                )
            })
            .unwrap_or_else(|| "tails unavailable".into());
        ui.label(
            RichText::new(format!("log return / bar · beyond axis: {} | {} · {tail}", a.outside[0], a.outside[1]))
                .small()
                .color(theme::muted()),
        )
        .math_card(|| tails_card(r, g));
    }
    ui.separator();
    let g = (*sel).min(2);
    let a = &f.ridges[g];
    ui.label(format!("BINNED ECDF · {} vs other classified", REGIMES[g]));
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width().max(80.), 92.), Sense::hover());
    for (cdf, col) in [(&a.cdf, regime_color(g)), (&a.complement_cdf, theme::muted())] {
        for k in 0..RIDGE_BINS {
            let x = rect.left() + k as f32 * rect.width() / 48.;
            let nx = rect.left() + (k + 1) as f32 * rect.width() / 48.;
            let y = rect.bottom() - cdf[k] as f32 * rect.height();
            let ny = rect.bottom() - cdf[k + 1] as f32 * rect.height();
            ui.painter().line_segment([pos2(x, y), pos2(nx, y)], Stroke::new(1., col));
            ui.painter().line_segment([pos2(nx, y), pos2(nx, ny)], Stroke::new(1., col));
        }
    }
    if let Some(d) = &a.distance {
        let x = rect.left() + ((d.at / f.half_width + 1.) * 0.5).clamp(0., 1.) as f32 * rect.width();
        ui.painter().line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1., theme::text()));
        ui.painter().text(
            rect.left_top(),
            egui::Align2::LEFT_TOP,
            format!("D = {:.3} · regime / other classified", d.d),
            theme::mono(11.),
            theme::text(),
        );
    }
    response.math_card(|| distance_card(r, g, rotation));
    ui.label(RichText::new("Descriptive history, not a forecast. Labels use only past bars. Binned ECDF is approximate; D uses exact observations.").small().color(theme::muted()));
    probe
}
