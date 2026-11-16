//! Presentation only: all returns, moments, bounds and envelopes arrive from the worker.
use super::*;
use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use tr_data::backtest_honesty::{self as h, Comparison, Estimate, Evaluation, FoldReport, HonestReport, Moments};

fn unavailable<T>(value: &Estimate<T>) -> String {
    value.reason().map(|r| format!("— {r}")).unwrap_or_default()
}
fn date(ts: i64) -> String {
    tr_core::ny::civil(ts).format("%Y-%m-%d %H:%M ET").to_string()
}
fn value(v: &Estimate<f64>, probability: bool) -> String {
    v.value()
        .map(|x| if probability { format!("{:.1}%", 100. * x) } else { format!("{x:+.2}") })
        .unwrap_or_else(|| unavailable(v))
}
fn probability_formula(m: &Moments, benchmark: f64) -> String {
    format!(
        "Φ(({:.8} − {:.8}) × √({} − 1) / √{:.8})\nV = 1 − ({:.8}) × ({:.8}) + ({:.8} − 1) × ({:.8})² / 4",
        m.sharpe, benchmark, m.n, m.probability_variance, m.skew, m.sharpe, m.pearson_kurtosis, m.sharpe
    )
}
/// Plain meaning of each probability. DSR is NOT "P(true Sharpe > 0)": it tests
/// against SR*, the Sharpe the luckiest of N_eff zero-skill variants would show.
pub(super) const PSR_MEANING: &str = "PSR: probability the true Sharpe > 0 (no deflation)";
pub(super) const DSR_MEANING: &str =
    "DSR: probability the true Sharpe beats SR*, the best Sharpe you'd expect from N_eff lucky zero-skill variants";
fn probability_card(ui: &mut Ui, c: Option<&Comparison>, h: &HonestReport, deflated: bool) {
    card(ui, c.map(|c| &c.moments), h, if deflated { "DSR = PSR(SR*)" } else { "PSR = PSR(0)" });
    ui.label(if deflated { DSR_MEANING } else { PSR_MEANING });
    ui.label("PSR(B) = Φ((SR − B) × √(n − 1) / √V); all Sharpe inputs are per session, not annualised.");
    if let Some(c) = c {
        ui.label(probability_formula(&c.moments, if deflated { c.null_maximum } else { 0. }));
        if deflated {
            if let Some(f) = h.family_statistics.value() {
                ui.label(
                    "SR* = expected maximum Sharpe of N_eff zero-skill trials; inputs use the common session dates.",
                );
                ui.label(if f.stochastic_trials == 1 {
                    "SR* = 0; one eligible trial has no cross-trial variance estimate."
                } else {
                    "SR* = √V[SR_k] × a_N; V[SR_k] = Σ(SR_k − mean SR)² / (M − 1)"
                });
                ui.label(match f.effective_trials {
                    1 => "a₁ = 0 (one effective trial: SR* = 0, DSR = PSR)".into(),
                    2 => "a₂ = 1 / √π (exact expected maximum of two)".into(),
                    n => format!("a_N = (1 − γ) × Φ⁻¹(1 − 1/{n}) + γ × Φ⁻¹(1 − 1/({n} × e)); γ = 0.5772156649"),
                });
                ui.label(format!(
                    "N_eff = {} · V[SR_k] = {:.10} (√ = {:.8}, M = {}) · γ = 0.5772156649\nSR* = {:.8} per session",
                    f.effective_trials,
                    f.sharpe_dispersion.powi(2),
                    f.sharpe_dispersion,
                    f.stochastic_trials,
                    c.null_maximum
                ));
            }
            ui.label(format!("DSR {}", value(&c.dsr, true)));
        } else {
            ui.label(format!("PSR {}", value(&c.psr, true)));
        }
    } else {
        ui.label("— see common-family reason in the pane");
    }
}
fn card(ui: &mut Ui, m: Option<&Moments>, h: &HonestReport, formula: &str) {
    ui.set_max_width(490.);
    ui.label(RichText::new(formula).font(mono(12.)).color(theme::cyan()));
    if let Some(m) = m {
        ui.label(format!("n={} sessions · mean={:.8} · sample SD={:.8}\nSR/session={:.6} · SR/year={:.4}\nskew={:.6} · Pearson kurtosis={:.6}\nV=1−skew×SR+(kurtosis−1)×SR²/4={:.6}",m.n,m.mean,m.sample_sd,m.sharpe,m.sharpe*h::YEAR.sqrt(),m.skew,m.pearson_kurtosis,m.probability_variance));
        if m.n < 30 {
            ui.label(RichText::new("LOW N · fewer than 30 sessions; no approval verdict").color(amber()));
        }
    }
    if let Some(f) = h.family_statistics.value() {
        ui.label(format!("Common dates {} … {} · n={}\nM={} stochastic · N_eff={} · excluded flat zero={}\nComplete linkage ρ ≥ .90 · sizes {:?}\nEmpirical trial SR SD={:.8} (session units)",f.dates.first().map_or("—",String::as_str),f.dates.last().map_or("—",String::as_str),f.dates.len(),f.stochastic_trials,f.effective_trials,f.excluded_zero_trials,f.cluster_sizes,f.sharpe_dispersion));
        if f.sharpe_dispersion == 0. {
            ui.label("Observed dispersion is zero; no positive floor is invented.");
        }
        ui.label(match f.effective_trials {
            1 => "Null branch: N=1, a₁=0; DSR=PSR",
            2 => "Null branch: N=2, a₂=1/√π (exact independent Gaussian maximum)",
            _ => "Null branch: Bailey–López de Prado Gaussian maximum approximation",
        });
    }
    if let Some(f) = h.trial_family.value() {
        ui.label(format!(
            "Scope {}\nTracked since {} · revision {} · N_raw={}",
            f.scope,
            date(f.tracked_since),
            f.revision,
            f.trials.len()
        ));
    }
    ui.label(format!("Closed through {}\n{}\n{}", date(h.closed_through), h.admission, h.execution));
    ui.label("PSR/DSR are asymptotic moment estimates. Session aggregation and purging do not establish IID returns. Earlier and external trials are unknown.");
}
fn fold_card(ui: &mut Ui, f: &FoldReport, h: &HonestReport) {
    ui.set_max_width(510.);
    ui.label(
        RichText::new(format!("TEST {} … {} · n={}", f.test.first_date, f.test.last_date, f.test_n))
            .color(theme::cyan()),
    );
    ui.label(format!("Candidate IS {} … {} · retained n={}\nPurged labels={} · retained spans={} · purge spans={}\nPrior embargo spans={} · nominal embargo end unit={} (may be beyond loaded history)",f.train_candidate.first_date,f.train_candidate.last_date,f.train_n,f.removed_labels,f.train_used.len(),f.purged.len(),f.prior_embargoes.len(),f.nominal_embargo_end));
    for label in f.purged_labels.iter().take(12) {
        ui.label(format!(
            "Removed label: decision {} · entry {} · exit {}",
            date(label.decision_ts),
            date(label.entry_ts),
            label.exit_ts.map(date).unwrap_or_else(|| "unknown (censored at train cutoff)".into())
        ));
    }
    if f.purged_labels.len() > 12 {
        ui.label("Further removed labels are included in EXPORT BACKTEST.");
    }
    for s in &f.train_used {
        ui.label(format!("Retained {} … {}", s.first_date, s.last_date));
    }
    for s in &f.purged {
        ui.label(format!("Removed {} … {}", s.first_date, s.last_date));
    }
    for s in &f.prior_embargoes {
        ui.label(format!("Prior embargo {} … {}", s.first_date, s.last_date));
    }
    if let Some(e) = &f.embargo {
        ui.label(format!("Post-test embargo {} … {}", e.first_date, e.last_date));
    }
    ui.label(format!("IS {} · OOS {}\nRetention {}", sharpe(&f.train), sharpe(&f.oos), value(&f.retention, false)));
    ui.label("Purge: [decision, exit] intersects test/embargo; unresolved exits are censored. Entire mixed accounting returns are removed. Retention=OOS SR / IS SR, only for positive IS; degradation=1−retention. Aggregate uses paired test-count weights, not pooled expanding IS observations.");
    card(ui, f.oos.value(), h, "Net rₜ=Eₜ/Eₜ₋₁−1; fold starts at E₋₁=1; test-end exit costs included");
}
fn sharpe(m: &Estimate<Moments>) -> String {
    m.value().map(|m| format!("{:+.2}", m.sharpe * h::YEAR.sqrt())).unwrap_or_else(|| unavailable(m))
}
fn sign(x: f64) -> Color32 {
    if x > 0. {
        theme::up()
    } else if x < 0. {
        theme::down()
    } else {
        muted()
    }
}
fn text_at(ui: &Ui, p: Pos2, anchor: Align2, s: impl ToString, size: f32, color: Color32) {
    ui.painter().text(p, anchor, s.to_string(), mono(size), color);
}
fn hatch(ui: &Ui, r: Rect, color: Color32) {
    let painter = ui.painter().with_clip_rect(r.intersect(ui.clip_rect()));
    let mut x = r.left() - r.height();
    while x < r.right() {
        painter.line_segment(
            [Pos2::new(x, r.bottom()), Pos2::new(x + r.height(), r.top())],
            Stroke::new(1., color.gamma_multiply(0.6)),
        );
        x += 9.;
    }
}
pub(super) fn render(ui: &mut Ui, h: &HonestReport, variant: &mut usize, fold: &mut usize) {
    ScrollArea::vertical()
        .id_salt("honest-evidence")
        .auto_shrink([false, false])
        .show(ui, |ui| render_body(ui, h, variant, fold));
}
fn render_body(ui: &mut Ui, h: &HonestReport, variant: &mut usize, fold: &mut usize) {
    ui.label(RichText::new("HONEST BACKTEST · historical OOS replay").font(mono(15.)).strong().color(theme::cyan()));
    ui.label(RichText::new(format!("{DSR_MEANING}.\n{PSR_MEANING}.")).color(muted()));
    ui.label(
        RichText::new(format!("VERIFIED · CLOSED THROUGH {}", date(h.closed_through))).font(mono(11.)).color(muted()),
    );
    ui.label(RichText::new(&h.admission).font(mono(11.)).color(muted()));
    ui.label(
        RichText::new(format!(
            "Accepted schedule: {} folds · {} embargo sessions · 0% risk-free · 252 sessions/year",
            h.settings.folds, h.settings.embargo_sessions
        ))
        .color(muted()),
    );
    if h.variants.is_empty() {
        ui.label("No evaluated variants");
        return;
    }
    *variant = (*variant).min(h.variants.len() - 1);
    ui.horizontal_wrapped(|ui| {
        for (i, v) in h.variants.iter().enumerate() {
            if ui.selectable_label(*variant == i, &v.label).clicked() {
                *variant = i;
                *fold = 0;
            }
        }
    });
    if let Some(f) = h.trial_family.value() {
        ui.label(RichText::new(format!("Tracked since {} · N_raw={} · revision {} · replayed on current history",date(f.tracked_since),f.trials.len(),f.revision)).font(mono(11.)).color(muted())).on_hover_text(format!("Scope: {}\nAutomatic benchmarks and legacy holdouts are included. Earlier/external research is unknown. Reopening cannot reset tracked trials.",f.scope));
    } else {
        ui.label(RichText::new(format!("N_raw=— · DSR {}", unavailable(&h.trial_family))).color(muted()));
    }
    let v = &h.variants[*variant];
    let Some(e) = v.evaluation.value() else {
        ui.label(RichText::new(unavailable(&v.evaluation)).color(muted()));
        return;
    };
    *fold = (*fold).min(e.folds.len().saturating_sub(1));
    let comparison = h.family_statistics.value().and_then(|f| f.comparisons.get(&v.id)).and_then(Estimate::value);
    ui.label(RichText::new(format!("Statistics for {} · selected variant", v.label)).font(mono(11.)).color(muted()));
    ui.horizontal_wrapped(|ui| {
        let sr = comparison.map(|c| format!("{:+.2}", c.moments.sharpe * h::YEAR.sqrt())).unwrap_or_else(|| "—".into());
        ui.label(RichText::new(format!("SR {sr}")).font(mono(23.)).color(if comparison.is_some() {
            text()
        } else {
            muted()
        }))
        .on_hover_ui(|ui| {
            card(ui, comparison.map(|c| &c.moments), h, "SR=mean / sample SD; annual SR=√252 × session SR")
        });
        ui.label(
            RichText::new(format!("PSR {}", comparison.map(|c| value(&c.psr, true)).unwrap_or_else(|| "—".into())))
                .font(mono(19.))
                .color(if comparison.is_some_and(|c| c.psr.value().is_some()) { theme::cyan() } else { muted() }),
        )
        .on_hover_ui(|ui| probability_card(ui, comparison, h, false));
        ui.label(
            RichText::new(format!("DSR {}", comparison.map(|c| value(&c.dsr, true)).unwrap_or_else(|| "—".into())))
                .font(mono(19.))
                .color(if comparison.is_some_and(|c| c.dsr.value().is_some()) { theme::cyan() } else { muted() }),
        )
        .on_hover_ui(|ui| probability_card(ui, comparison, h, true));
    });
    if let Some(f) = h.family_statistics.value() {
        ui.label(
            RichText::new(format!(
                "COMMON COMPARISON · n={} · {} … {} · N_eff={} / {} stochastic · cluster sizes {:?}",
                f.dates.len(),
                f.dates.first().map_or("—", String::as_str),
                f.dates.last().map_or("—", String::as_str),
                f.effective_trials,
                f.stochastic_trials,
                f.cluster_sizes
            ))
            .color(muted())
            .font(mono(11.)),
        );
        if let Some(reason) = f.comparisons.get(&v.id).and_then(Estimate::reason) {
            ui.label(RichText::new(format!("DSR — {reason}")).color(muted()));
        }
    } else {
        ui.label(RichText::new("COMMON COMPARISON · n=— · dates — · N_eff=—").font(mono(11.)).color(muted()));
        ui.label(RichText::new(format!("DSR {}", unavailable(&h.family_statistics))).color(muted()));
    }
    ui.label(
        RichText::new(format!(
            "FULL STITCHED OOS · n={} · {} … {} · SR {} · max DD {:.1}%",
            e.returns.len(),
            e.returns.first().map_or("—", |p| p.session_date.as_str()),
            e.returns.last().map_or("—", |p| p.session_date.as_str()),
            sharpe(&e.full_oos),
            100. * e.max_drawdown
        ))
        .font(mono(12.))
        .color(text()),
    )
    .on_hover_ui(|ui| {
        ui.label(format!(
            "Full OOS dates: {} … {}",
            e.returns.first().map_or("—", |p| p.session_date.as_str()),
            e.returns.last().map_or("—", |p| p.session_date.as_str())
        ));
        card(
            ui,
            e.full_oos.value(),
            h,
            "G_f,t = G_previous_end × e_f,t; test observations compounded once; embargo has no returns",
        );
    });
    let benchmark = h.variants.iter().find(|v| v.benchmark).and_then(|v| v.evaluation.value()).filter(|b| {
        b.folds.len() == e.folds.len() && b.folds.iter().zip(&e.folds).all(|(a, b)| a.test.indices == b.test.indices)
    });
    if benchmark.is_none() {
        ui.label(
            RichText::new("Benchmark has a different warm-up/fold schedule; select it separately.")
                .font(mono(11.))
                .color(muted()),
        );
    }
    equity(ui, e, benchmark, *fold);
    ribbon(ui, e, fold, h);
    gauge(ui, comparison, h);
    scatter(ui, e, fold, h);
    ui.label(RichText::new(format!("Paired fold retention {}", value(&e.retention, false))).color(muted()))
        .on_hover_ui(|ui| {
            card(
                ui,
                None,
                h,
                "Q=Σ(n_test×SR_OOS) / Σ(n_test×SR_IS); D=1−Q; all paired IS denominators must be positive",
            )
        });
    for (i, f) in e.folds.iter().enumerate() {
        let label = format!(
            "{}  {} … {} · IS {} · OOS {} · purge {} · retention {}",
            i + 1,
            f.test.first_date,
            f.test.last_date,
            sharpe(&f.train),
            sharpe(&f.oos),
            f.train_candidate.indices.end - f.train_candidate.indices.start - f.train_n,
            value(&f.retention, false)
        );
        if ui
            .selectable_label(*fold == i, RichText::new(label).font(mono(12.)))
            .on_hover_ui(|ui| fold_card(ui, f, h))
            .clicked()
        {
            *fold = i;
        }
    }
    ui.label(RichText::new("Train ░   Purge ////   Test ±   Embargo //// · PageUp/Down/Home/End select a fold when this pane has focus").font(mono(10.5)).color(muted()));
    ui.label(RichText::new(&h.execution).font(ui_font(11.)).color(muted()));
}

fn equity(ui: &mut Ui, e: &Evaluation, benchmark: Option<&Evaluation>, selected: usize) {
    let (r, response) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(80.), 190.), Sense::hover());
    ui.painter().rect_filled(r, 4., theme::card());
    let plot = r.shrink2(Vec2::new(38., 20.));
    let Some(first) = e.folds.first() else { return };
    let last = e.folds.last().unwrap();
    let a = first.train_candidate.start;
    let b = last.test.end;
    let x = |ts: i64| plot.left() + (ts - a) as f32 / (b - a).max(1) as f32 * plot.width();
    let lo = benchmark.map_or(e.equity_bounds[0], |b| b.equity_bounds[0].min(e.equity_bounds[0]));
    let hi = benchmark.map_or(e.equity_bounds[1], |b| b.equity_bounds[1].max(e.equity_bounds[1]));
    let y = |v: f64| plot.bottom() - ((v - lo) / (hi - lo).max(0.01)) as f32 * plot.height();
    for i in 0..3 {
        let yy = plot.top() + i as f32 * plot.height() / 2.;
        ui.painter().hline(plot.x_range(), yy, Stroke::new(1., theme::grid()));
    }
    ui.painter().hline(plot.x_range(), y(1.), Stroke::new(1., muted()));
    for (i, f) in e.folds.iter().enumerate() {
        let rect = Rect::from_min_max(Pos2::new(x(f.test.start), plot.top()), Pos2::new(x(f.test.end), plot.bottom()));
        ui.painter().rect_filled(rect, 0., sign(f.net_return).gamma_multiply(if i == selected { 0.12 } else { 0.04 }));
        if i == selected {
            ui.painter().rect_stroke(rect, 0., Stroke::new(1., theme::cyan()), egui::StrokeKind::Inside);
        }
    }
    let draw = |tape: &Evaluation, color: Color32, thick: f32, dashed: bool| {
        for (i, w) in tape.equity.windows(2).enumerate() {
            if w[1].break_before || (dashed && i % 4 >= 2) {
                continue;
            }
            ui.painter().line_segment(
                [Pos2::new(x(w[0].ts), y(w[0].equity)), Pos2::new(x(w[1].ts), y(w[1].equity))],
                Stroke::new(thick, color),
            );
        }
    };
    if let Some(b) = benchmark {
        draw(b, muted(), 1., true);
    }
    draw(e, theme::cyan(), 2.5, false);
    text_at(
        ui,
        r.left_top() + Vec2::new(6., 4.),
        Align2::LEFT_TOP,
        if benchmark.is_some() {
            "OOS EQUITY · unit capital · dashed benchmark"
        } else {
            "OOS EQUITY · unit capital"
        },
        11.,
        muted(),
    );
    text_at(ui, Pos2::new(r.left() + 4., plot.top()), Align2::LEFT_TOP, format!("{hi:.2}"), 10., muted());
    text_at(ui, Pos2::new(r.left() + 4., plot.bottom()), Align2::LEFT_BOTTOM, format!("{lo:.2}"), 10., muted());
    text_at(ui, Pos2::new(plot.right(), r.bottom() - 2.), Align2::RIGHT_BOTTOM, &last.test.last_date, 10., muted());
    response.on_hover_text("Only test-fold returns are compounded. Each fold starts flat with unit funding. Gaps in the line mark fold boundaries / embargo; no synthetic flat investment return is inserted.");
}
fn ribbon(ui: &mut Ui, e: &Evaluation, selected: &mut usize, h: &HonestReport) {
    let height = 28. + e.folds.len() as f32 * 23.;
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(80.), height), Sense::hover());
    // A holdout-shaped or hand-edited evaluation can carry no folds; never panic in paint.
    let (Some(first), Some(last)) = (e.folds.first(), e.folds.last()) else {
        return;
    };
    let plot = r.shrink2(Vec2::new(38., 0.));
    let a = first.train_candidate.start;
    let b = last.test.end;
    let x = |ts: i64| plot.left() + (ts - a) as f32 / (b - a).max(1) as f32 * plot.width();
    text_at(
        ui,
        r.left_top(),
        Align2::LEFT_TOP,
        "FOLD RIBBON  train ░ · purge //// · test ± · embargo ////",
        11.,
        muted(),
    );
    for (i, f) in e.folds.iter().enumerate() {
        let top = r.top() + 25. + i as f32 * 23.;
        text_at(ui, Pos2::new(r.left(), top + 7.), Align2::LEFT_CENTER, format!("F{}", i + 1), 11., text());
        let rect = |s: &h::Span| {
            Rect::from_min_max(Pos2::new(x(s.start), top), Pos2::new(x(s.end).max(x(s.start) + 2.), top + 15.))
        };
        ui.painter().rect_filled(rect(&f.train_candidate), 1., theme::cyan().gamma_multiply(0.15));
        for p in &f.purged {
            let p = rect(p);
            ui.painter().rect_filled(p, 0., amber().gamma_multiply(0.18));
            hatch(ui, p, amber());
        }
        for p in &f.prior_embargoes {
            hatch(ui, rect(p), muted());
        }
        if let Some(p) = &f.embargo {
            hatch(ui, rect(p), muted());
        }
        let test = rect(&f.test);
        ui.painter().rect_filled(test, 2., sign(f.net_return).gamma_multiply(0.6));
        text_at(
            ui,
            test.center(),
            Align2::CENTER_CENTER,
            format!("{}  {:+.1}%", i + 1, f.net_return * 100.),
            10.,
            text(),
        );
        if *selected == i {
            ui.painter().rect_stroke(test, 2., Stroke::new(2., theme::cyan()), egui::StrokeKind::Inside);
        }
        let row = Rect::from_min_max(Pos2::new(r.left(), top), Pos2::new(r.right(), top + 20.));
        if ui
            .interact(row, ui.id().with(("fold-ribbon", i)), Sense::click())
            .on_hover_ui(|ui| fold_card(ui, f, h))
            .clicked()
        {
            *selected = i;
        }
    }
}
fn gauge(ui: &mut Ui, c: Option<&Comparison>, h: &HonestReport) {
    let (r, response) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(80.), 135.), Sense::hover());
    ui.painter().rect_filled(r, 4., theme::card());
    let Some(c) = c else {
        text_at(ui, r.center(), Align2::CENTER_CENTER, "DSR — see common-family reason above", 12., muted());
        return;
    };
    let Some(dsr) = c.dsr.value() else {
        text_at(ui, r.center(), Align2::CENTER_CENTER, format!("DSR {}", unavailable(&c.dsr)), 12., muted());
        return;
    };
    let bar = Rect::from_min_max(r.left_top() + Vec2::new(18., 32.), Pos2::new(r.right() - 18., r.top() + 50.));
    text_at(
        ui,
        r.left_top() + Vec2::new(18., 5.),
        Align2::LEFT_TOP,
        format!("DSR {:.1}%  {}", dsr * 100., if c.moments.n < 30 { "LOW N" } else { "" }),
        18.,
        theme::cyan(),
    );
    ui.painter().rect_filled(bar, 2., theme::grid());
    let fill = Rect::from_min_max(bar.min, Pos2::new(bar.left() + *dsr as f32 * bar.width(), bar.bottom()));
    ui.painter().rect_filled(fill, 2., theme::cyan().gamma_multiply(0.7));
    if c.moments.n < 30 {
        hatch(ui, fill, amber());
    }
    for p in [0.5, 0.95, 0.99] {
        let xx = bar.left() + p * bar.width();
        ui.painter().vline(xx, bar.top()..=bar.bottom() + 4., Stroke::new(1., text()));
        text_at(ui, Pos2::new(xx, bar.bottom() + 5.), Align2::CENTER_TOP, format!("{p:.2}"), 9., muted());
    }
    let raw = c.moments.sharpe * h::YEAR.sqrt();
    let null = c.null_maximum * h::YEAR.sqrt();
    let lo = raw.min(null).min(0.);
    let hi = raw.max(null).max(0.);
    let x = |v: f64| bar.left() + ((v - lo) / (hi - lo).max(0.01)) as f32 * bar.width();
    ui.painter().hline(bar.x_range(), r.top() + 91., Stroke::new(2., muted()));
    ui.painter().line_segment(
        [Pos2::new(x(raw), r.top() + 91.), Pos2::new(x(null), r.top() + 91.)],
        Stroke::new(6., theme::cyan().gamma_multiply(0.4)),
    );
    for (v, color) in [(0., muted()), (raw, theme::cyan()), (null, amber())] {
        ui.painter().vline(x(v), (r.top() + 82.)..=(r.top() + 100.), Stroke::new(2., color));
    }
    text_at(
        ui,
        Pos2::new(bar.left(), r.bottom() - 7.),
        Align2::LEFT_BOTTOM,
        format!("SR {raw:+.2} · SR* {null:+.2} · SR − SR* {:+.2} (annual)", c.sharpe_above_null * h::YEAR.sqrt()),
        11.,
        text(),
    );
    response.on_hover_ui(|ui| {
        ui.label(format!(
            "SR* (expected best of N_eff lucky zero-skill trials) = {:.8} per session · annual={:.5}\nSR − SR* = {:.8} session units",
            c.null_maximum, null, c.sharpe_above_null
        ));
        probability_card(ui, Some(c), h, true);
    });
}
fn scatter(ui: &mut Ui, e: &Evaluation, selected: &mut usize, h: &HonestReport) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(80.), 210.), Sense::hover());
    ui.painter().rect_filled(r, 4., theme::card());
    let plot = r.shrink2(Vec2::new(40., 30.));
    let bound = e.scatter_bound;
    let x = |v: f64| plot.left() + ((v + bound) / (2. * bound)) as f32 * plot.width();
    let y = |v: f64| plot.bottom() - ((v + bound) / (2. * bound)) as f32 * plot.height();
    ui.painter()
        .line_segment([Pos2::new(x(-bound), y(-bound)), Pos2::new(x(bound), y(bound))], Stroke::new(1., muted()));
    ui.painter().hline(plot.x_range(), y(0.), Stroke::new(1., theme::grid()));
    ui.painter().vline(x(0.), plot.y_range(), Stroke::new(1., theme::grid()));
    text_at(
        ui,
        r.left_top() + Vec2::new(8., 4.),
        Align2::LEFT_TOP,
        "IS vs OOS · annual SR · equality diagonal",
        12.,
        muted(),
    );
    text_at(
        ui,
        Pos2::new(r.center().x, r.bottom() - 3.),
        Align2::CENTER_BOTTOM,
        format!("IS  [{:+.1}, {:+.1}]", -bound, bound),
        10.,
        muted(),
    );
    text_at(ui, Pos2::new(r.left() + 3., r.center().y), Align2::LEFT_CENTER, "OOS", 10., muted());
    for (i, f) in e.folds.iter().enumerate() {
        let (Some(a), Some(b)) = (f.train.value(), f.oos.value()) else {
            continue;
        };
        let p = Pos2::new(x(a.sharpe * h::YEAR.sqrt()), y(b.sharpe * h::YEAR.sqrt()));
        ui.painter().circle_filled(p, if *selected == i { 8. } else { 6. }, sign(b.sharpe));
        if *selected == i {
            ui.painter().circle_stroke(p, 10., Stroke::new(2., theme::cyan()));
        }
        text_at(ui, p + Vec2::new(9., -9.), Align2::LEFT_BOTTOM, i + 1, 11., text());
        if ui
            .interact(Rect::from_center_size(p, Vec2::splat(24.)), ui.id().with(("fold-point", i)), Sense::click())
            .on_hover_ui(|ui| fold_card(ui, f, h))
            .clicked()
        {
            *selected = i;
        }
    }
    for (i, f) in e.folds.iter().enumerate().filter(|(_, f)| f.train.value().is_none() || f.oos.value().is_none()) {
        ui.label(
            RichText::new(format!("F{} scatter unavailable · IS {} · OOS {}", i + 1, sharpe(&f.train), sharpe(&f.oos)))
                .color(muted())
                .font(mono(11.)),
        );
    }
}
impl TerminalApp {
    pub(in crate::app) fn backtest_keys(&mut self, ctx: &egui::Context) -> bool {
        if ctx.text_edit_focused()
            || !self.cmd.is_empty()
            || self.focus_cmd
            || self.palette.open
            || self.rebinding.is_some()
            || ctx.memory(|m| m.has_focus(egui::Id::new("command-line")))
            || self.watch_ui.picker.is_some()
            || self.watch_ui.delete.is_some()
            || self.watch_ui.edit.is_some()
        {
            return false;
        }
        let keys = [egui::Key::PageUp, egui::Key::PageDown, egui::Key::Home, egui::Key::End];
        let key = keys.into_iter().find(|k| {
            ctx.input(|i| i.key_pressed(*k) && !i.modifiers.any())
                && self.prefs.hotkeys.action_for(*k, egui::Modifiers::NONE).is_none_or(|action| {
                    // End's default JumpLatest binding applies to charts. Borrow
                    // it in BT; an explicitly reassigned chord keeps priority.
                    *k == egui::Key::End
                        && action == crate::prefs::Action::JumpLatest
                        && self.prefs.hotkeys.0.get(&action).is_some_and(|chord| chord == "End")
                })
        });
        let Some(key) = key else {
            return false;
        };
        let Some(PaneKind::Backtest(pane)) = self.panes.iter_mut().find(|p| p.id == self.focus).map(|p| &mut p.kind)
        else {
            return false;
        };
        let h = pane
            .report
            .as_ref()
            .and_then(|r| r.honesty.as_ref())
            .or_else(|| pane.intraday_report.as_ref().and_then(|r| r.honesty.as_ref()));
        let n = h
            .and_then(|h| h.variants.get(pane.selected_variant))
            .and_then(|v| v.evaluation.value())
            .map_or(0, |e| e.folds.len());
        if n == 0 || pane.holdout_tables {
            return false;
        }
        pane.selected_fold = match key {
            egui::Key::PageUp => pane.selected_fold.saturating_sub(1),
            egui::Key::PageDown => (pane.selected_fold + 1).min(n - 1),
            egui::Key::Home => 0,
            _ => n - 1,
        };
        ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paint_text(output: &egui::FullOutput) -> String {
        output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(t.galley.job.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn honest_backtest_selected_trial_counts_definition_and_unavailable_evidence() {
        let report = super::super::tests::honest_report();
        let mut honest = report.honesty.as_ref().unwrap().clone();
        // Force different probabilities to catch accidental reuse of the first
        // (benchmark) comparison when inspecting another selected experiment.
        if let Estimate::Available(f) = &mut honest.family_statistics {
            for (v, probability) in honest.variants.iter().zip([0.123, 0.876]) {
                if let Some(Estimate::Available(c)) = f.comparisons.get_mut(&v.id) {
                    c.dsr = Estimate::Available(probability);
                }
            }
        }
        let ctx = egui::Context::default();
        for (mut selected, expected, other) in [(0, "DSR 12.3%", "DSR 87.6%"), (1, "DSR 87.6%", "DSR 12.3%")] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 2200.))),
                    ..Default::default()
                },
                |ui| render_body(ui, &honest, &mut selected, &mut 0),
            );
            let text = paint_text(&output);
            for expected in [expected, "N_raw=4", "N_eff=", "n=", DSR_MEANING, PSR_MEANING, "selected variant"] {
                assert!(text.contains(expected), "missing {expected}");
            }
            assert!(!text.contains(other));
            // DSR is a test against SR*, never "P(true Sharpe > 0)".
            assert!(DSR_MEANING.contains("beats SR*") && !DSR_MEANING.contains("> 0"));
            assert!(!text.contains("DSR: estimated probability true Sharpe > 0"));
            output.textures_delta.clear();
        }
        honest.family_statistics = Estimate::Unavailable(h::Reason::NoComparableTrialWindow);
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 2200.))),
                ..Default::default()
            },
            |ui| render_body(ui, &honest, &mut 1, &mut 0),
        );
        let text = paint_text(&output);
        assert!(text.contains("DSR — insufficient common trial dates"));
        assert!(text.contains("n=— · dates — · N_eff=—"));
        assert!(!text.contains("DSR 87.6%"));
        for shape in &output.shapes {
            if let egui::Shape::Text(t) = &shape.shape
                && t.galley.job.text.starts_with("DSR —")
            {
                assert!(t.galley.job.sections.iter().all(|s| s.format.color == muted()));
            }
        }
        output.textures_delta.clear();
    }
    #[test]
    fn honest_backtest_math_cards_show_selected_inputs_and_deflated_benchmark() {
        let report = super::super::tests::honest_report();
        let honest = report.honesty.as_ref().unwrap();
        let m = h::moments(&[0., 0.01, 0.01, 0.02]).unwrap();
        let null = h::expected_maximum(2, 0.1).unwrap();
        let c = Comparison {
            moments: m,
            psr: h::psr(&m, 0.).into(),
            dsr: h::psr(&m, null).into(),
            null_maximum: null,
            sharpe_above_null: m.sharpe - null,
        };
        let ctx = egui::Context::default();
        for (deflated, benchmark, result) in [(false, "0.00000000", "PSR 96.5%"), (true, "0.05641896", "DSR 95.8%")] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 2200.))),
                    ..Default::default()
                },
                |ui| probability_card(ui, Some(&c), honest, deflated),
            );
            let text = paint_text(&output);
            assert!(text.contains(&format!("Φ((1.22474487 − {benchmark}) × √(4 − 1) / √1.37500000)")));
            assert!(text.contains("2.00000000 − 1")); // Pearson, not excess kurtosis.
            assert!(text.contains(result));
            if deflated {
                assert!(text.contains(DSR_MEANING));
                assert!(!text.contains(PSR_MEANING));
                // SR* is shown with every input: N_eff, cross-trial variance and γ.
                let f = honest.family_statistics.value().unwrap();
                for input in [
                    format!("N_eff = {}", f.effective_trials),
                    format!("V[SR_k] = {:.10}", f.sharpe_dispersion.powi(2)),
                    "γ = 0.5772156649".into(),
                    "SR* = 0.05641896 per session".into(),
                ] {
                    assert!(text.contains(&input), "missing {input}");
                }
            } else {
                assert!(text.contains(PSR_MEANING));
                assert!(!text.contains("SR* = 0.05641896"));
            }
            output.textures_delta.clear();
        }
        let mut single = honest.clone();
        if let Estimate::Available(f) = &mut single.family_statistics {
            f.stochastic_trials = 1;
            f.effective_trials = 1;
            f.sharpe_dispersion = 0.;
        }
        let c = Comparison { null_maximum: 0., dsr: c.psr.clone(), ..c };
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 2200.))),
                ..Default::default()
            },
            |ui| probability_card(ui, Some(&c), &single, true),
        );
        let text = paint_text(&output);
        assert!(text.contains("a₁ = 0"));
        assert!(text.contains("one eligible trial has no cross-trial variance estimate"));
        assert!(!text.contains("/ (M − 1)"));
        output.textures_delta.clear();
    }
    #[test]
    fn honest_backtest_paint_survives_a_variant_without_folds() {
        let report = super::super::tests::honest_report();
        let mut honest = report.honesty.as_ref().unwrap().clone();
        if let Estimate::Available(e) = &mut honest.variants[0].evaluation {
            e.folds.clear();
        }
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 2200.))),
                ..Default::default()
            },
            |ui| render_body(ui, &honest, &mut 0, &mut 0),
        );
        assert!(paint_text(&output).contains("FULL STITCHED OOS"));
        output.textures_delta.clear();
    }
    fn click(ctx: &egui::Context, p: Pos2, mut paint: impl FnMut(&mut Ui)) {
        for pressed in [true, false] {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 600.))),
                    events: vec![
                        egui::Event::PointerMoved(p),
                        egui::Event::PointerButton {
                            pos: p,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |ui| paint(ui),
            );
            out.textures_delta.clear();
        }
    }
    #[test]
    fn honest_backtest_ribbon_and_scatter_share_fold_selection() {
        let report = super::super::tests::honest_report();
        let honest = report.honesty.as_ref().unwrap();
        let eval = honest.variants[1].evaluation.value().unwrap();
        let ctx = egui::Context::default();
        let mut selected = 0;
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 600.))),
                ..Default::default()
            },
            |ui| ribbon(ui, eval, &mut selected, honest),
        );
        let row = out
            .shapes
            .iter()
            .find_map(|s| {
                if let egui::Shape::Text(t) = &s.shape {
                    t.galley.job.text.starts_with("2  ").then(|| t.pos + t.galley.size() / 2.)
                } else {
                    None
                }
            })
            .expect("fold 2 ribbon label");
        out.textures_delta.clear();
        click(&ctx, row, |ui| ribbon(ui, eval, &mut selected, honest));
        assert_eq!(selected, 1);
        selected = 0;
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 600.))),
                ..Default::default()
            },
            |ui| scatter(ui, eval, &mut selected, honest),
        );
        let points: Vec<_> = out
            .shapes
            .iter()
            .filter_map(
                |s| if let egui::Shape::Circle(c) = &s.shape { (c.radius == 6.).then_some(c.center) } else { None },
            )
            .collect();
        assert_eq!(points.len(), 2); // fold 1 is the selected, larger circle
        let point = points[0];
        out.textures_delta.clear();
        click(&ctx, point, |ui| scatter(ui, eval, &mut selected, honest));
        assert_eq!(selected, 1);
    }
}
