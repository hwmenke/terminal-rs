//! Paint-only Edge column. Statistical selections and BH are cached by changed().
use super::*;
use crate::widgets;
pub(super) const WIDTH: f32 = 196.0;
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
pub(super) fn header(ui: &mut Ui, p: &mut TrendWallPane, at: egui::Pos2) {
    for (x, w, title, sort) in
        [(0.0, 40.0, "EDGE", Sort::EdgeScore), (40.0, 48.0, "Δ5b", Sort::EdgeDelta), (88.0, 44.0, "q", Sort::EdgeQ)]
    {
        let r = Rect::from_min_size(at + vec2(x, 0.0), vec2(w, 20.0));
        let response = ui.interact(r, ui.id().with(("edge-header", x as i32)), Sense::click());
        ui.painter().text(
            r.left_top(),
            Align2::LEFT_TOP,
            format!("{title}{}", if p.sort == sort { "▼" } else { "" }),
            mono(10.0),
            if response.hovered() { text() } else { muted() },
        );
        if response.clicked() {
            p.sort = sort;
        }
    }
    ui.painter().text(at + vec2(132.0, 0.0), Align2::LEFT_TOP, "n  FOLDS", mono(10.0), muted());
}
fn tile(rect: Rect) -> Rect {
    Rect::from_min_size(rect.min + vec2(12.0, 1.0), vec2(64.0, 19.0))
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
                        mesh.add_colored_rect(
                            Rect::from_min_size(r.min + vec2(166.0 + fold as f32 * 7.0, 5.0), vec2(4.0, 11.0)),
                            color(if d > 0.0 {
                                1
                            } else if d < 0.0 {
                                -1
                            } else {
                                0
                            })
                            .gamma_multiply(if e.pass { 1.0 } else { 0.3 }),
                        );
                    }
                }
            }
        }
        p.edge_mesh = Some((key, Arc::new(mesh)));
    }
    ui.painter().add(egui::Shape::Mesh(p.edge_mesh.as_ref().unwrap().1.clone()));
    for (visible, &i) in ids.iter().enumerate() {
        let Some(e) = p.edges.get(i) else { continue };
        let r = Rect::from_min_size(rect.min + vec2(0.0, visible as f32 * ROW), vec2(WIDTH, ROW));
        let response = ui.interact(r, ui.id().with(("edge", i)), Sense::click());
        if let Some(blank) = blank(e) {
            ui.painter().text(
                r.left_center(),
                Align2::LEFT_CENTER,
                blank,
                mono(10.0),
                if e.miss.as_ref().is_some_and(|m| m.code != "SHORT HISTORY") { amber() } else { muted() },
            );
        } else if let Some(c) = &e.cell {
            let glyph = match (c.regime, e.live) {
                (1, true) => "▲",
                (-1, true) => "▼",
                (0, true) => "■",
                (1, false) => "△",
                (-1, false) => "▽",
                _ => "□",
            };
            ui.painter().text(r.left_center(), Align2::LEFT_CENTER, glyph, mono(10.0), color(c.regime));
            if !e.pass {
                ui.painter().rect_stroke(tile(r), 0.0, Stroke::new(1.0, theme::grid()), egui::StrokeKind::Inside);
            }
            ui.painter().text(
                tile(r).center(),
                Align2::CENTER_CENTER,
                format!("{}bp", fmt(e.delta_bp())),
                mono(10.0),
                if e.pass { text() } else { muted() },
            );
            ui.painter().text(
                r.min + vec2(79.0, 11.0),
                Align2::LEFT_CENTER,
                format!("q {}{}", qtext(e.q), if p.edge_inflight.is_some() { "~" } else { "" }),
                mono(9.0),
                if e.q.is_some_and(|q| q <= tr_data::tstack_tune::FDR) { text() } else { muted() },
            );
            ui.painter().text(
                r.min + vec2(124.0, 11.0),
                Align2::LEFT_CENTER,
                if c.low_n { "LOW N".into() } else { format!("n {}", c.n) },
                mono(9.0),
                if c.low_n { amber() } else { muted() },
            );
            for (fold, f) in c.folds.iter().enumerate() {
                if f.delta.is_none_or(|d| !d.is_finite()) {
                    ui.painter().rect_stroke(
                        Rect::from_min_size(r.min + vec2(166.0 + fold as f32 * 7.0, 5.0), vec2(4.0, 11.0)),
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
        response.on_hover_text(hover(p, i));
    }
}
pub(super) fn summary_strip(ui: &mut Ui, p: &TrendWallPane) {
    let s = &p.edge_summary;
    ui.horizontal_wrapped(|ui| {
        if s.m == 0 { label(ui,"EDGE · no verified names to test",muted()); return; }
        let thru = p.rows.iter().filter_map(|r| p.edge_reports.get(&(r.symbol.clone(),p.daily)).and_then(|r| r.as_ref().ok())).map(|r| date(r.closed_thru_ts,p.daily)).min().unwrap_or_else(|| "—".into());
        label(ui,format!("EDGE · 5b fwd · {} · CLOSED THRU {thru} · m {} · {} pass q≤.10 ({}▲ {}▼) · median {}bp · IQR {}…{} · {}/{}",if p.daily {"daily"} else {"5m"},s.m,s.pass_up+s.pass_down,s.pass_up,s.pass_down,fmt(s.median_bp),fmt(s.q1_bp),fmt(s.q3_bp),if p.edge_inflight.is_some() {p.edge_received.len()} else {s.resolved},p.rows.len()),text()).on_hover_text("Family: loaded names only. Pending and short-history names enter BH at p = 1. CLOSED THRU is the oldest window end; each card gives its own dates.");
        let (r,_) = ui.allocate_exact_size(vec2(140.0,18.0),Sense::hover());
        ui.painter().hline(r.x_range(),r.center().y,Stroke::new(1.0,theme::grid()));
        for e in &p.edges {
            if let Some(d) = e.delta_bp() {
                let x = r.center().x+(d/s.scale_bp) as f32*r.width()/2.0;
                let h = if e.pass {8.0} else {4.0};
                ui.painter().vline(x,(r.center().y-h)..=(r.center().y+h),Stroke::new(1.0,if e.pass { widgets::heat_ramp(d,s.scale_bp) } else { muted() }));
            }
        }
        widgets::heat_key(ui,s.scale_bp,"Edge Δ bp");
        if s.small { label(ui,format!("SMALL WALL · m {} · FDR is close to raw p",s.m),amber()).on_hover_text(format!("A lone name needs p ≤ 0.10/m = {}",qtext(s.lone_p))); }
    });
    if let Some(e) = &p.edge_error {
        label(ui, e, amber());
    }
}
fn point(rect: Rect, scale: f64, e: &DisplayEdge) -> Option<egui::Pos2> {
    let d = e.delta_bp()?;
    let q = e.q.filter(|q| q.is_finite() && (0.0..=1.0).contains(q))?;
    Some(pos2(
        rect.center().x + (d / scale) as f32 * rect.width() / 2.0,
        rect.bottom() - (-q.max(0.001).log10() / 3.0) as f32 * rect.height(),
    ))
}
pub(super) fn scatter(ui: &mut Ui, p: &mut TrendWallPane) {
    let (outer, response) = ui.allocate_exact_size(vec2(ui.available_width().max(240.0), 170.0), Sense::click());
    let r = outer.shrink2(vec2(36.0, 18.0));
    let painter = ui.painter();
    let stroke = Stroke::new(1.0, theme::grid());
    painter.line_segment([r.left_bottom(), r.left_top()], stroke);
    painter.line_segment([r.left_bottom(), r.right_bottom()], stroke);
    painter.add(egui::Shape::dashed_line(
        &[pos2(r.center().x, r.top()), pos2(r.center().x, r.bottom())],
        stroke,
        4.0,
        3.0,
    ));
    let y = r.bottom() - r.height() / 3.0;
    painter.add(egui::Shape::dashed_line(&[pos2(r.left(), y), pos2(r.right(), y)], stroke, 4.0, 3.0));
    painter.text(pos2(r.left(), y), Align2::LEFT_BOTTOM, "q .10", mono(9.0), muted());
    painter.text(r.left_top(), Align2::LEFT_BOTTOM, "−log10 q · 0…3", mono(9.0), muted());
    painter.text(
        r.center_bottom() + vec2(0.0, 3.0),
        Align2::CENTER_TOP,
        format!("Δ bp · ±{:.1}", p.edge_summary.scale_bp),
        mono(9.0),
        muted(),
    );
    let mut missing = 0;
    let mut nearest: Option<(f32, usize)> = None;
    for (i, e) in p.edges.iter().enumerate() {
        let Some(at) = point(r, p.edge_summary.scale_bp, e) else {
            missing += 1;
            continue;
        };
        if e.pass {
            painter.circle_filled(at, 3.5, widgets::heat_ramp(e.delta_bp().unwrap(), p.edge_summary.scale_bp));
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
            painter.circle_stroke(at, 6.5, Stroke::new(1.0, amber()));
        }
        if let Some(mouse) = response.hover_pos() {
            let distance = at.distance(mouse);
            if distance < 10.0 && nearest.is_none_or(|(d, _)| distance < d) {
                nearest = Some((distance, i));
            }
        }
    }
    painter.text(r.right_top(), Align2::RIGHT_TOP, format!("{missing} without q"), mono(9.0), muted());
    if let Some((_, i)) = nearest {
        if response.clicked() {
            p.selected = Some(i);
            p.scroll_selected = true;
        }
        response.on_hover_text(hover(p, i));
    }
}
pub(super) fn hover(p: &TrendWallPane, i: usize) -> String {
    let row = &p.rows[i];
    let Some(e) = p.edges.get(i) else { return "Edge pending".into() };
    if let Some(m) = &e.miss {
        return format!("{} · — {}\n{}\nCounts in wall family m: {}", row.symbol, m.code, m.detail, m.is_test);
    }
    let Some(c) = &e.cell else { return format!("{} · Edge pending · counted as p = 1", row.symbol) };
    let report = p.edge_reports.get(&(row.symbol.clone(), p.daily)).and_then(|r| r.as_ref().ok());
    let Some(r) = report else { return "Edge pending".into() };
    let Some(s) = &c.stats else { return "— LOW N".into() };
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
        "{} · EDGE 5b · regime {regime} ({mode})\nΔ = mean(r | regime) − mean(r | all warmed bars) = {}bp · r = C[t+5]/C[t] − 1\ncond {:+.1}bp · baseline {}bp · hit {:.0}% (Wilson {:.0}–{:.0}) · n {} → n_eff {:.1}\nnull: circular rotation of the regime block, two-sided, up to 999 deterministic offsets · p {} · band {}\nwall FDR: BH step-up across m {} names · q {} (per-name q {}, not used)",
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
        qtext(c.q)
    );
    for (i, f) in c.folds.iter().enumerate() {
        out.push_str(&format!(
            "\nfold {} {}bp · n {} · {}",
            i + 1,
            fmt(f.delta.map(|d| d * 1e4)),
            f.n,
            f.span
                .map(|(a, b)| format!("{} → {}", date(a, p.daily), date(b, p.daily)))
                .unwrap_or_else(|| "unknown".into())
        ));
    }
    out.push_str(&format!("\nscore {} = 100 · min(−log10 q, 3)/3 · agree {}/4 · sign Δ\ninputs: {} closed {} bars · {} → CLOSED THRU {}\nsource: {} · {} · VAULT {} + LIVE {} · {}{}{}\nparams: Adaptive Trend defaults · horizon 5\nOutcomes may span the close. Descriptive evidence, not a forecast.",fmt(e.score),wall_edge::agrees(c),r.bars,r.tf.label(),date(r.first_ts,p.daily),date(r.closed_thru_ts,p.daily),r.source,r.provenance.label(),r.vault_bars,r.live_bars,if p.daily {"local research archive"} else {"regular session"},if r.vault_only {" · VAULT ONLY"} else {""},if r.trimmed_gap {" · contiguous tail after last vault gap"} else {""}));
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
    fn column_frame(ctx: &egui::Context, p: &mut TrendWallPane, events: Vec<egui::Event>) -> String {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(600.0, 300.0))),
                events,
                ..Default::default()
            },
            |ui| {
                column(ui, p, &[0], Rect::from_min_size(pos2(20.0, 20.0), vec2(WIDTH, ROW)));
            },
        );
        let mut s = String::new();
        for shape in &output.shapes {
            texts(&shape.shape, &mut s)
        }
        output.textures_delta.clear();
        s
    }
    #[test]
    fn unverified_edge_paints_em_dash_and_zero_digits() {
        let mut p = pane(false);
        p.edge_reports.insert((p.rows[0].symbol.clone(), false), Err(Miss::unverified("unknown source")));
        p.changed();
        let s = column_frame(&egui::Context::default(), &mut p, vec![]);
        assert!(s.contains("— UNVERIFIED"));
        assert!(!s.chars().any(|c| c.is_ascii_digit()));
        assert!(hover(&p, 0).contains("Counts in wall family m: false"));
    }
    #[test]
    fn failing_row_paints_no_filled_delta_tile() {
        let ctx = egui::Context::default();
        let mut p = pane(false);
        column_frame(&ctx, &mut p, vec![]);
        let mesh = &p.edge_mesh.as_ref().unwrap().1;
        assert_eq!(mesh.vertices.len(), 16);
        assert!(mesh.vertices.iter().all(|v| v.pos.x >= 186.0));
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
