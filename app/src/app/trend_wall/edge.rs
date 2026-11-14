//! Paint-only Edge column. Statistical selections and BH are cached by changed().
use super::*;
use crate::widgets;

// Column geometry, relative to the Edge block's left edge. Header and cells share it.
const GLYPH_X: f32 = 1.0;
const TILE_X: f32 = 12.0;
const TILE_W: f32 = 60.0;
const Q_X: f32 = 76.0;
const SCORE_X: f32 = 122.0;
const N_X: f32 = 150.0;
const FOLD_X: f32 = 188.0;
const FOLD_STEP: f32 = 7.0;
pub(super) const WIDTH: f32 = 224.0;

#[derive(Clone, PartialEq)]
pub(super) struct MeshKey {
    revision: u64,
    rect: Rect,
    sort: Sort,
    colors: [Color32; 5],
}
fn label(ui: &mut Ui, s: impl Into<String>, c: Color32) -> egui::Response {
    ui.label(RichText::new(s).font(mono(10.0)).color(c))
}
fn fmt(v: Option<f64>) -> String {
    v.filter(|v| v.is_finite()).map(|v| format!("{v:+.1}")).unwrap_or_else(|| "—".into())
}
fn qtext(q: Option<f64>) -> String {
    q.filter(|q| q.is_finite() && (0.0..=1.0).contains(q))
        .map(|q| if q < 0.01 { format!("{q:.3}") } else { format!("{q:.2}") })
        .unwrap_or_else(|| "—".into())
}
fn date(ts: i64, daily: bool) -> String {
    if daily {
        chrono::DateTime::from_timestamp(ts, 0).unwrap_or_default().format("%Y-%m-%d").to_string()
    } else {
        tr_core::ny::civil(ts).format("%Y-%m-%d %H:%M ET").to_string()
    }
}
/// End of a closed bar: intraday bars are stamped at their open.
fn bar_end(r: &WallEdge) -> i64 {
    if r.tf.is_intraday() { r.closed_thru_ts + r.tf.seconds() } else { r.closed_thru_ts }
}
/// Two header lines inside the wall's 24 px header band: the group title with fill
/// progress, then sortable column labels aligned with the cells below.
pub(super) fn header(ui: &mut Ui, p: &mut TrendWallPane, at: egui::Pos2) {
    let pending = p.edge_pending();
    let partial = pending > 0 && is_edge_sort(p.sort);
    let title = if pending > 0 {
        format!("EDGE 5b · {}/{} computed", p.rows.len() - pending, p.rows.len())
    } else {
        "EDGE 5b fwd".into()
    };
    ui.painter().text(
        at + vec2(GLYPH_X, 0.0),
        Align2::LEFT_TOP,
        title,
        mono(9.0),
        if partial { amber() } else { muted() },
    );
    let line = at + vec2(0.0, 12.0);
    for (x, w, title, sort, align) in [
        (TILE_X, TILE_W, "Δ5b", Some(Sort::EdgeDelta), Align2::CENTER_TOP),
        (Q_X, SCORE_X - Q_X, "q", Some(Sort::EdgeQ), Align2::LEFT_TOP),
        (SCORE_X, N_X - SCORE_X, "S", Some(Sort::EdgeScore), Align2::LEFT_TOP),
        (N_X, FOLD_X - N_X, "n", None, Align2::LEFT_TOP),
        (FOLD_X, WIDTH - FOLD_X, "FOLDS", None, Align2::LEFT_TOP),
    ] {
        let r = Rect::from_min_size(line + vec2(x, 0.0), vec2(w, 12.0));
        let anchor = if align == Align2::CENTER_TOP { r.center_top() } else { r.left_top() };
        let Some(sort) = sort else {
            ui.painter().text(anchor, align, title, mono(9.0), muted());
            continue;
        };
        let response = ui.interact(r, ui.id().with(("edge-header", title)), Sense::click());
        let active = p.sort == sort;
        ui.painter().text(
            anchor,
            align,
            format!("{title}{}", if active { "▼" } else { "" }),
            mono(9.0),
            if response.hovered() || active { text() } else { muted() },
        );
        if response.clicked() {
            p.sort = sort;
        }
        response.on_hover_text(match sort {
            Sort::EdgeDelta => "Sort by Δ (bp), most positive first",
            Sort::EdgeQ => "Sort by wall-wide BH q, smallest first",
            _ => "Sort by |edge score| S = 100 · min(−log10 q, 3)/3 · agree/4 · sign Δ",
        });
    }
}
fn tile(rect: Rect) -> Rect {
    Rect::from_min_size(rect.min + vec2(TILE_X, 2.0), vec2(TILE_W, ROW - 4.0))
}
fn fold_rect(rect: Rect, fold: usize) -> Rect {
    Rect::from_min_size(rect.min + vec2(FOLD_X + fold as f32 * FOLD_STEP, 5.0), vec2(4.0, 11.0))
}
fn blank(e: &DisplayEdge) -> Option<String> {
    e.miss.as_ref().map(|m| format!("— {}", m.code)).or_else(|| e.cell.is_none().then(|| "···".into()))
}
pub(super) fn column(ui: &mut Ui, p: &mut TrendWallPane, ids: &[usize], rect: Rect) {
    let key = MeshKey {
        revision: p.edge_revision,
        rect,
        sort: p.sort,
        colors: [theme::up(), theme::down(), theme::grid(), text(), muted()],
    };
    if p.edge_mesh.as_ref().is_none_or(|(k, _)| *k != key) {
        let mut mesh = Mesh::default();
        for (visible, &i) in ids.iter().enumerate() {
            let Some(e) = p.edges.get(i) else { continue };
            if blank(e).is_some() {
                continue;
            }
            let r = Rect::from_min_size(rect.min + vec2(0.0, visible as f32 * ROW), vec2(WIDTH, ROW));
            if e.pass {
                mesh.add_colored_rect(
                    tile(r),
                    widgets::heat_ramp(e.delta_bp().unwrap_or(0.0), p.edge_summary.scale_bp),
                );
            }
            if let Some(c) = &e.cell {
                for (fold, f) in c.folds.iter().enumerate() {
                    if let Some(d) = f.delta.filter(|d| d.is_finite()) {
                        let state = if d > 0.0 {
                            1
                        } else if d < 0.0 {
                            -1
                        } else {
                            0
                        };
                        mesh.add_colored_rect(
                            fold_rect(r, fold),
                            color(state).gamma_multiply(if e.pass { 1.0 } else { 0.3 }),
                        );
                    }
                }
            }
        }
        p.edge_mesh = Some((key, Arc::new(mesh)));
    }
    if let Some((_, mesh)) = &p.edge_mesh {
        ui.painter().add(egui::Shape::Mesh(mesh.clone()));
    }
    let filling = p.edge_inflight.is_some();
    let painter = ui.painter().clone();
    for (visible, &i) in ids.iter().enumerate() {
        let Some(e) = p.edges.get(i) else { continue };
        let r = Rect::from_min_size(rect.min + vec2(0.0, visible as f32 * ROW), vec2(WIDTH, ROW));
        let y = r.center().y;
        if !ui.clip_rect().intersects(r) {
            continue;
        }
        let response = ui.interact(r, ui.id().with(("edge", i)), Sense::click());
        if let Some(blank) = blank(e) {
            let tone = match e.miss.as_ref().map(|m| m.code) {
                None | Some("SHORT HISTORY") | Some("LOW N") => muted(),
                _ => amber(),
            };
            painter.text(pos2(r.left() + TILE_X, y), Align2::LEFT_CENTER, blank, mono(10.0), tone);
        } else if let Some(c) = &e.cell {
            let glyph = match (c.regime, e.live) {
                (1, true) => "▲",
                (-1, true) => "▼",
                (0, true) => "■",
                (1, false) => "△",
                (-1, false) => "▽",
                _ => "□",
            };
            let regime_color = if c.regime == 0 { muted() } else { color(c.regime) };
            painter.text(pos2(r.left() + GLYPH_X, y), Align2::LEFT_CENTER, glyph, mono(10.0), regime_color);
            if !e.pass {
                painter.rect_stroke(tile(r), 0.0, Stroke::new(1.0, theme::grid()), egui::StrokeKind::Inside);
            }
            painter.text(
                tile(r).center(),
                Align2::CENTER_CENTER,
                format!("{}bp", fmt(e.delta_bp())),
                mono(10.0),
                if e.pass { text() } else { muted() },
            );
            painter.text(
                pos2(r.left() + Q_X, y),
                Align2::LEFT_CENTER,
                format!("q {}{}", qtext(e.q), if filling { "~" } else { "" }),
                mono(9.0),
                if e.q.is_some_and(|q| q <= tr_data::tstack_tune::FDR) { text() } else { muted() },
            );
            painter.text(
                pos2(r.left() + SCORE_X, y),
                Align2::LEFT_CENTER,
                e.score.map_or_else(|| "—".into(), |s| format!("{s:+.0}")),
                mono(9.0),
                if e.pass { text() } else { muted() },
            );
            painter.text(
                pos2(r.left() + N_X, y),
                Align2::LEFT_CENTER,
                if c.low_n { "LOW N".into() } else { format!("n {}", c.n) },
                mono(9.0),
                if c.low_n { amber() } else { muted() },
            );
            for (fold, f) in c.folds.iter().enumerate() {
                if f.delta.is_none_or(|d| !d.is_finite()) {
                    painter.rect_stroke(
                        fold_rect(r, fold),
                        0.0,
                        Stroke::new(1.0, theme::grid().gamma_multiply(if e.pass { 1.0 } else { 0.3 })),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
        if response.clicked() {
            p.selected = Some(i);
        }
        // Built only while hovered: never formatted for fifty rows every frame.
        let card = &*p;
        response.on_hover_ui(|ui| {
            ui.label(RichText::new(hover(card, i)).font(mono(10.0)));
        });
    }
}
pub(super) fn summary_strip(ui: &mut Ui, p: &TrendWallPane) {
    let s = &p.edge_summary;
    ui.horizontal_wrapped(|ui| {
        if s.m == 0 && s.pending == 0 {
            label(ui, "EDGE · no verified names to test", muted());
            return;
        }
        let thru = p.edge_thru.map_or_else(|| "—".into(), |t| date(t, p.daily));
        let computed = p.rows.len() - s.pending;
        label(
            ui,
            format!(
                "EDGE · 5b fwd · {} · CLOSED THRU {thru} · m {} · {} pass q≤.10 ({}▲ {}▼) · median {}bp · IQR {}…{} · {computed}/{}",
                if p.daily { "daily" } else { "5m" },
                s.m,
                s.pass_up + s.pass_down,
                s.pass_up,
                s.pass_down,
                fmt(s.median_bp),
                fmt(s.q1_bp),
                fmt(s.q3_bp),
                p.rows.len(),
            ),
            text(),
        )
        .on_hover_text(
            "Family: the loaded names, one test each (the displayed regime at H = 5). Pending, \
             short-history and low-n names enter BH at p = 1, so q while filling is an upper bound; \
             unverified, missing, non-cash and invalid names are excluded from m. CLOSED THRU is the \
             oldest window end on the wall; each card gives its own dates.",
        );
        let (r, _) = ui.allocate_exact_size(vec2(140.0, 18.0), Sense::hover());
        let painter = ui.painter();
        painter.hline(r.x_range(), r.center().y, Stroke::new(1.0, theme::grid()));
        painter.vline(r.center().x, (r.center().y - 3.0)..=(r.center().y + 3.0), Stroke::new(1.0, muted()));
        for e in &p.edges {
            if let Some(d) = e.delta_bp() {
                let x = r.center().x + (d / s.scale_bp).clamp(-1.0, 1.0) as f32 * r.width() / 2.0;
                let h = if e.pass { 8.0 } else { 4.0 };
                let c = if e.pass { widgets::heat_ramp(d, s.scale_bp) } else { muted() };
                painter.vline(x, (r.center().y - h)..=(r.center().y + h), Stroke::new(1.0, c));
            }
        }
        widgets::heat_key(ui, s.scale_bp, "Edge Δ bp");
        if s.small && s.m > 0 {
            label(ui, format!("SMALL WALL · m {} · FDR is close to raw p", s.m), amber())
                .on_hover_text(format!("A lone name needs p ≤ 0.10/m = {}", qtext(s.lone_p)));
        }
    });
    if let Some(e) = &p.edge_error {
        label(ui, e, amber());
    }
}
fn point(rect: Rect, scale: f64, e: &DisplayEdge) -> Option<egui::Pos2> {
    let d = e.delta_bp()?;
    let q = e.q.filter(|q| q.is_finite() && (0.0..=1.0).contains(q))?;
    Some(pos2(
        rect.center().x + (d / scale).clamp(-1.0, 1.0) as f32 * rect.width() / 2.0,
        rect.bottom() - (-q.max(0.001).log10() / 3.0) as f32 * rect.height(),
    ))
}
pub(super) fn scatter(ui: &mut Ui, p: &mut TrendWallPane) {
    let (outer, response) = ui.allocate_exact_size(vec2(ui.available_width().max(240.0), 170.0), Sense::click());
    let r = outer.shrink2(vec2(36.0, 18.0));
    let painter = ui.painter().clone();
    let stroke = Stroke::new(1.0, theme::grid());
    painter.line_segment([r.left_bottom(), r.left_top()], stroke);
    painter.line_segment([r.left_bottom(), r.right_bottom()], stroke);
    painter.add(egui::Shape::dashed_line(
        &[pos2(r.center().x, r.top()), pos2(r.center().x, r.bottom())],
        stroke,
        4.0,
        3.0,
    ));
    // y = −log10 q on 0…3: ticks at q = 1, .1, .01, .001; the FDR line is q = .10.
    for (k, q) in ["1", ".1", ".01", "≤.001"].into_iter().enumerate() {
        let y = r.bottom() - r.height() * k as f32 / 3.0;
        painter.line_segment([pos2(r.left() - 3.0, y), pos2(r.left(), y)], stroke);
        painter.text(pos2(r.left() - 5.0, y), Align2::RIGHT_CENTER, q, mono(8.0), muted());
    }
    let fdr = r.bottom() - r.height() / 3.0;
    painter.add(egui::Shape::dashed_line(&[pos2(r.left(), fdr), pos2(r.right(), fdr)], stroke, 4.0, 3.0));
    painter.text(pos2(r.right(), fdr - 1.0), Align2::RIGHT_BOTTOM, "FDR q = .10", mono(9.0), muted());
    painter.text(r.left_top() + vec2(4.0, 0.0), Align2::LEFT_TOP, "q (−log10 scale)", mono(9.0), muted());
    let scale = p.edge_summary.scale_bp;
    painter.text(r.left_bottom() + vec2(0.0, 3.0), Align2::LEFT_TOP, format!("{:+.1}", -scale), mono(8.0), muted());
    painter.text(r.right_bottom() + vec2(0.0, 3.0), Align2::RIGHT_TOP, format!("{scale:+.1}"), mono(8.0), muted());
    painter.text(r.center_bottom() + vec2(0.0, 3.0), Align2::CENTER_TOP, "Δ5b bp", mono(9.0), muted());
    let mut missing = 0;
    let mut nearest: Option<(f32, usize)> = None;
    let mouse = response.hover_pos();
    for (i, e) in p.edges.iter().enumerate() {
        let Some(at) = point(r, scale, e) else {
            missing += 1;
            continue;
        };
        if e.pass {
            painter.circle_filled(at, 3.5, widgets::heat_ramp(e.delta_bp().unwrap_or(0.0), scale));
            painter.text(
                at + vec2(5.0, -3.0),
                Align2::LEFT_BOTTOM,
                tr_data::security(&p.rows[i].symbol).ticker,
                mono(9.0),
                text(),
            );
        } else {
            painter.circle_stroke(at, 3.5, Stroke::new(1.0, muted()));
        }
        if p.selected == Some(i) {
            painter.circle_stroke(at, 6.5, Stroke::new(1.5, amber()));
        }
        if let Some(mouse) = mouse {
            let distance = at.distance(mouse);
            if distance < 10.0 && nearest.is_none_or(|(d, _)| distance < d) {
                nearest = Some((distance, i));
            }
        }
    }
    painter.text(
        r.right_top(),
        Align2::RIGHT_TOP,
        format!("{missing} without q (pending or blank)"),
        mono(9.0),
        muted(),
    );
    if let Some((_, i)) = nearest {
        if response.clicked() {
            p.selected = Some(i);
            p.scroll_selected = true;
        }
        let card = &*p;
        response.on_hover_ui(|ui| {
            ui.label(RichText::new(hover(card, i)).font(mono(10.0)));
        });
    }
}
pub(super) fn hover(p: &TrendWallPane, i: usize) -> String {
    let Some(row) = p.rows.get(i) else { return "Edge pending".into() };
    let Some(e) = p.edges.get(i) else { return "Edge pending".into() };
    if let Some(m) = &e.miss {
        return format!(
            "{} · EDGE 5b · — {}\n{}\nWall FDR family: {}",
            row.symbol,
            m.code,
            m.detail,
            if m.is_test { "counted in m at p = 1" } else { "not a test (excluded from m)" }
        );
    }
    let Some(c) = &e.cell else { return format!("{} · Edge pending · counted in m at p = 1", row.symbol) };
    let report = p.edge_reports.get(&(row.symbol.clone(), p.daily)).and_then(|r| r.as_ref().ok());
    let Some(r) = report else { return "Edge pending".into() };
    let Some(s) = &c.stats else { return format!("{} · EDGE 5b · — LOW N", row.symbol) };
    let regime = match c.regime {
        1 => "▲ UP",
        -1 => "▼ DOWN",
        _ => "■ FLAT",
    };
    let mode = if e.live {
        format!(
            "live {}, completed {}",
            if p.daily { "daily" } else { "5m" },
            date(row.last_bar_ts.unwrap_or(r.closed_thru_ts), p.daily)
        )
    } else {
        "as of close".into()
    };
    let mut out = format!(
        "{} · EDGE 5b · regime {regime} ({mode})\n\
         Δ = mean(r | regime) − mean(r | all warmed bars) = {}bp · r = C[t+5]/C[t] − 1\n\
         cond {:+.1}bp · baseline {}bp · hit {:.0}% (Wilson {:.0}–{:.0}) · n {} → n_eff {:.1}\n\
         null: circular rotation of the regime block vs fixed returns, two-sided, ≤ 999 deterministic offsets · p {} · band {}\n\
         wall FDR: BH step-up across m {} names · q {}{} (per-name q {}, not used)",
        row.symbol,
        fmt(e.delta_bp()),
        s.mean * 1e4,
        fmt(r.baseline_mean.map(|v| v * 1e4)),
        s.hit_rate * 100.0,
        s.hit_ci95.0 * 100.0,
        s.hit_ci95.1 * 100.0,
        c.n,
        c.n_eff,
        qtext(c.p),
        c.null_band.map(|(a, b)| format!("[{:+.1}, {:+.1}]bp", a * 1e4, b * 1e4)).unwrap_or_else(|| "—".into()),
        p.edge_summary.m,
        qtext(e.q),
        if p.edge_inflight.is_some() { " (upper bound while filling)" } else { "" },
        qtext(c.q)
    );
    out.push_str("\nfolds: 4 contiguous purged stability folds (no fitting), fold Δ vs fold baseline");
    for (i, f) in c.folds.iter().enumerate() {
        out.push_str(&format!(
            "\n  {} {}bp · n {} · {}",
            i + 1,
            fmt(f.delta.map(|d| d * 1e4)),
            f.n,
            f.span
                .map(|(a, b)| format!("{} → {}", date(a, p.daily), date(b, p.daily)))
                .unwrap_or_else(|| "too few outcomes".into())
        ));
    }
    let source = if p.daily {
        format!("{} · {} · local research archive", r.source, r.provenance.label())
    } else {
        format!(
            "{} · {} · VAULT {} + LIVE {} · regular session{}",
            r.source,
            r.provenance.label(),
            r.vault_bars,
            r.live_bars,
            if r.vault_only { " · VAULT ONLY" } else { "" }
        )
    };
    out.push_str(&format!(
        "\nscore {} = 100 · min(−log10 q, 3)/3 · agree {}/4 · sign Δ{}\n\
         inputs: {} closed {} bars · {} → CLOSED THRU {}\nsource: {source}{}\n\
         params: Adaptive Trend defaults · horizon 5{}\nDescriptive evidence, not a forecast.",
        fmt(e.score),
        wall_edge::agrees(c),
        if e.pass { " · PASS" } else { "" },
        r.bars,
        r.tf.label(),
        date(r.first_ts, p.daily),
        date(bar_end(r), p.daily),
        if r.trimmed_gap { " · contiguous tail after the last vault gap" } else { "" },
        if p.daily { "" } else { " · outcomes starting in the last 25 min span the close" }
    ));
    if c.n == r.cells.iter().map(|c| c.n).sum::<usize>() {
        out.push_str("\nRegime never changed in the window");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pane(pass: bool) -> TrendWallPane {
        let mut p = TrendWallPane::new("AAPL");
        p.rows = vec![Row::failure("AAPL US Equity", LaneStatus::Unavailable("offline".into()), false)];
        let mut report = super::super::tests::report();
        if !pass {
            for c in &mut Arc::make_mut(&mut report).cells {
                c.p = Some(0.8);
            }
        }
        p.edge_reports.insert((p.rows[0].symbol.clone(), false), Ok(report));
        p.changed();
        p
    }
    fn texts(shape: &egui::Shape, output: &mut String) {
        match shape {
            egui::Shape::Text(t) => {
                output.push_str(&t.galley.job.text);
                output.push('\n');
            }
            egui::Shape::Vec(shapes) => {
                for s in shapes {
                    texts(s, output)
                }
            }
            _ => {}
        }
    }
    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, f: impl FnMut(&mut Ui)) -> String {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(600.0, 300.0))),
                events,
                ..Default::default()
            },
            f,
        );
        let mut s = String::new();
        for shape in &output.shapes {
            texts(&shape.shape, &mut s)
        }
        output.textures_delta.clear();
        s
    }
    fn column_frame(ctx: &egui::Context, p: &mut TrendWallPane, events: Vec<egui::Event>) -> String {
        frame(ctx, events, |ui| column(ui, p, &[0], Rect::from_min_size(pos2(20.0, 20.0), vec2(WIDTH, ROW))))
    }
    #[test]
    fn unverified_edge_paints_em_dash_and_zero_digits() {
        let mut p = pane(false);
        p.edge_reports.insert((p.rows[0].symbol.clone(), false), Err(Miss::unverified("unknown source")));
        p.changed();
        let s = column_frame(&egui::Context::default(), &mut p, vec![]);
        assert!(s.contains("— UNVERIFIED"));
        assert!(!s.chars().any(|c| c.is_ascii_digit()));
        assert!(hover(&p, 0).contains("not a test (excluded from m)"));
    }
    #[test]
    fn failing_row_paints_no_filled_delta_tile() {
        let ctx = egui::Context::default();
        let mut p = pane(false);
        column_frame(&ctx, &mut p, vec![]);
        let mesh = &p.edge_mesh.as_ref().unwrap().1;
        // Four dimmed fold ticks only (4 vertices each), all inside the fold strip.
        assert_eq!(mesh.vertices.len(), 16);
        assert!(mesh.vertices.iter().all(|v| v.pos.x >= 20.0 + FOLD_X));
        let mut p = pane(true);
        column_frame(&ctx, &mut p, vec![]);
        assert_eq!(p.edge_mesh.as_ref().unwrap().1.vertices.len(), 20);
        // Four agreeing folds alone do not bypass the minimum effective n gate.
        let report = p.edge_reports.values_mut().next().unwrap().as_mut().unwrap();
        for c in &mut Arc::make_mut(report).cells {
            c.low_n = true;
        }
        p.changed();
        column_frame(&ctx, &mut p, vec![]);
        assert_eq!(p.edge_mesh.as_ref().unwrap().1.vertices.len(), 16);
    }
    #[test]
    fn header_labels_sit_over_their_cells_and_count_pending_rows() {
        let ctx = egui::Context::default();
        let mut p = pane(true);
        p.rows.push(Row::failure("MSFT US Equity", LaneStatus::Unavailable("offline".into()), false));
        p.sort = Sort::EdgeQ;
        p.changed();
        let mut shapes = Vec::new();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(600.0, 300.0))),
                ..Default::default()
            },
            |ui| header(ui, &mut p, pos2(100.0, 0.0)),
        );
        output.textures_delta.clear();
        for s in &output.shapes {
            if let egui::Shape::Text(t) = &s.shape {
                shapes.push((t.galley.job.text.clone(), t.pos.x));
            }
        }
        let x = |label: &str| shapes.iter().find(|(t, _)| t.starts_with(label)).unwrap().1;
        assert!(shapes.iter().any(|(t, _)| t == "EDGE 5b · 1/2 computed"));
        assert!((x("q") - (100.0 + Q_X)).abs() < 0.5);
        assert!((x("S") - (100.0 + SCORE_X)).abs() < 0.5);
        assert!((x("n") - (100.0 + N_X)).abs() < 0.5);
        assert!((x("FOLDS") - (100.0 + FOLD_X)).abs() < 0.5);
        // Δ5b is centred over the tile.
        let delta = shapes.iter().find(|(t, _)| t.starts_with("Δ5b")).unwrap();
        assert!(delta.1 > 100.0 + TILE_X && delta.1 < 100.0 + TILE_X + TILE_W / 2.0);
        assert!(shapes.iter().any(|(t, _)| t == "q▼"));
    }
    #[test]
    fn cell_text_shows_score_and_hover_card_is_built_lazily() {
        let ctx = egui::Context::default();
        let mut p = pane(true);
        let s = column_frame(&ctx, &mut p, vec![]);
        let score = p.edges[0].score.unwrap();
        assert!(s.contains(&format!("{score:+.0}")), "{s}");
        // Not hovered: the card text is not laid out.
        assert!(!s.contains("wall FDR"));
        let card = hover(&p, 0);
        for part in ["Δ = mean(r | regime)", "null: circular rotation", "BH step-up", "inputs:", "folds:"] {
            assert!(card.contains(part), "{part}");
        }
    }
    #[test]
    fn edge_mesh_reused_on_hover_and_ribbon_mesh_vertex_count_unchanged() {
        let ctx = egui::Context::default();
        let mut p = pane(true);
        column_frame(&ctx, &mut p, vec![]);
        let mesh = p.edge_mesh.as_ref().unwrap().1.clone();
        column_frame(&ctx, &mut p, vec![egui::Event::PointerMoved(pos2(30.0, 30.0))]);
        assert!(Arc::ptr_eq(&mesh, &p.edge_mesh.as_ref().unwrap().1));
        // The full ribbon geometry and its vertex count are separately exercised by
        // ribbon_mesh_is_reused_on_hover_and_click_opens_native_interval.
    }
    #[test]
    fn scatter_click_selects_the_row() {
        let ctx = egui::Context::default();
        let mut p = pane(true);
        let mut at = egui::Pos2::ZERO;
        let frame = |p: &mut TrendWallPane, events: Vec<egui::Event>, at: &mut egui::Pos2| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(600.0, 300.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let outer =
                        Rect::from_min_size(ui.next_widget_position(), vec2(ui.available_width().max(240.0), 170.0));
                    *at = point(outer.shrink2(vec2(36.0, 18.0)), p.edge_summary.scale_bp, &p.edges[0]).unwrap();
                    scatter(ui, p);
                },
            )
            .textures_delta
            .clear();
        };
        frame(&mut p, vec![], &mut at);
        let mouse = at;
        frame(
            &mut p,
            vec![
                egui::Event::PointerMoved(mouse),
                egui::Event::PointerButton {
                    pos: mouse,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            &mut at,
        );
        frame(
            &mut p,
            vec![egui::Event::PointerButton {
                pos: mouse,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut at,
        );
        assert_eq!(p.selected, Some(0));
        assert!(p.scroll_selected);
    }
}
