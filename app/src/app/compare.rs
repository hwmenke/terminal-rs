//! Intraday multi-symbol compare (`ICMP`): percent from each name's 09:30 open.
use super::{PaneKind, TerminalApp};
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use tr_data::intraday_compare::{self, CompareChart, SymbolSeries};

pub struct ComparePane {
    pub symbol: String,
    pub peers: String,
    pub tf: String,
    pub generation: u64,
    pub chart: Option<CompareChart>,
    pub loading: bool,
    pub error: Option<String>,
    /// A typo in the request (bar size, names…): a "check the inputs" card, not a failed run.
    pub input_error: Option<String>,
    /// Text-box buffer of the ticker chooser; the real `symbol` is only set on commit. Not persisted.
    pub ticker_draft: String,
}

impl ComparePane {
    pub fn new(symbol: impl Into<String>, peers: impl Into<String>) -> Self {
        let symbol = symbol.into();
        let symbol = if symbol.trim().is_empty() { symbol } else { tr_data::security(&symbol).key() };
        Self { symbol, peers: peers.into(), tf: "5m".into(), generation: 0, chart: None, loading: false, error: None, input_error: None, ticker_draft: String::new() }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        let symbol = self.symbol.trim();
        if symbol.is_empty() {
            // Needs input, not an error: the pane shows a ticker chooser.
            self.loading = false;
            return;
        }
        if let Err(err) = intraday_compare::parse_query(symbol, &self.peers, &self.tf) {
            self.input_error = Some(err);
            self.loading = false;
            return;
        }
        self.input_error = None;
        // A kept snapshot belongs to one ticker: never show it under another name.
        // Nor under another bar size or peer set.
        let query = intraday_compare::parse_query(symbol, &self.peers, &self.tf).ok();
        let key_set = |names: &mut dyn Iterator<Item = &String>, anchor: &str| {
            let mut keys: Vec<String> = names.map(|n| tr_data::security(n).key()).collect();
            keys.push(tr_data::security(anchor).key());
            keys.sort();
            keys.dedup();
            keys
        };
        let stale = |c: &CompareChart| {
            let Some(q) = query.as_ref() else { return true };
            c.timeframe != q.timeframe
                || key_set(&mut c.series.iter().map(|s| &s.symbol), &c.anchor)
                    != key_set(&mut q.symbols.iter(), &q.anchor)
        };
        if self.chart.as_ref().is_some_and(|c| !crate::widgets::same_security(&c.anchor, symbol) || stale(c)) {
            // (stale covers bar size and peer set)
            self.chart = None;
        }
        self.generation += 1;
        self.loading = true;
        self.error = None;
        worker.send(Request::IntradayCompare {
            pane,
            generation: self.generation,
            symbol: tr_data::security(symbol).key(),
            spec: self.peers.clone(),
            timeframe: self.tf.clone(),
        });
    }

    pub fn accept(&mut self, generation: u64, result: Result<CompareChart, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(chart) if !chart.provenance.is_verified() => {
                self.chart = None;
                self.error = Some(format!(
                    "Intraday compare refused: {}. Verified real data is required.",
                    chart.provenance.label()
                ));
            }
            Ok(chart) => {
                self.tf = chart.timeframe.label().into();
                self.chart = Some(chart);
                self.error = None;
            }
            // Keep the previous snapshot; run_state labels it as such.
            Err(err) => {
                self.error = Some(err);
            }
        }
    }
}

impl TerminalApp {
    pub(super) fn open_intraday_compare(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        // No focused ticker is a setup state (a ticker chooser), not an error.
        let pane = ComparePane::new(symbol, spec);
        let ticker = if pane.symbol.is_empty() { "Compare".into() } else { tr_data::security(&pane.symbol).ticker };
        self.status = format!("{ticker}: intraday compare, percent from each 09:30 open");
        self.open_panel(PaneKind::IntradayCompare(Box::new(pane)));
    }

    pub(super) fn compare_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("compare pane");
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        let mut reload = false;
        let watchlist = self.watchlist.clone();
        {
            let PaneKind::IntradayCompare(pane) = &mut self.panes[i].kind else { return vec![] };
            if !pane.loading && pane.chart.is_none() && pane.error.is_none() && pane.input_error.is_none() && !pane.symbol.trim().is_empty() {
                pane.request(id, &self.worker);
            }
            let has_symbol = !pane.symbol.trim().is_empty();
            let context = if let Some(c) = &pane.chart {
                c.anchor_ticker.clone()
            } else if has_symbol {
                tr_data::security(&pane.symbol).ticker
            } else { "% from 09:30 open".into() };
            // Intraday results carry no structured receipt, so the chip is the honest compact legacy line.
            let legacy = pane.chart.as_ref().map(|c| {
                format!("{} · {} · {} · {}", c.date, c.timeframe.label(), c.provenance.label(), c.source)
            });
            let source = legacy.as_deref().map(|l| crate::widgets::HeaderSource { lead: "", parts: &[], legacy: l });
            crate::widgets::pane_header(ui, "Session compare", &context, source, |ui| {
                if has_symbol
                    && crate::widgets::header_button(ui, "Reload", "Read verified regular-session bars again", false)
                        .clicked()
                {
                    reload = true;
                }
            });
            if !has_symbol {
                let what = "Compare one name against peers as percent from each 09:30 open, on real intraday bars.";
                if let Some(key) = crate::widgets::needs_ticker(ui, what, &mut pane.ticker_draft, &watchlist) {
                    pane.symbol = key;
                    pane.ticker_draft.clear();
                    pane.request(id, &self.worker);
                }
                return Vec::new();
            }
            crate::widgets::methodology(
                ui,
                (id, "icmp-method"),
                "Each line is that name's close divided by its own 09:30 open. The zero line is the open. SPY is on the same clock. A missing bucket is a gap. Premarket and after-hours prints stay off the chart.",
            );
            if sample_offline {
                crate::widgets::chip(
                    ui,
                    crate::widgets::ChipKind::Neutral,
                    "Sample display mode: this pane needs live research data and never uses sample bars",
                );
            }
            ui.horizontal_wrapped(|ui| {
                if crate::widgets::anchor_field(ui, "Anchor", &mut pane.ticker_draft, &pane.symbol) {
                    reload = true;
                }
                ui.label(RichText::new("Names").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.peers).desired_width(180.0).hint_text("MSFT NVDA"));
                ui.label(RichText::new("Bars").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.tf).desired_width(48.0).hint_text("5m"));
                if ui.button(RichText::new("RUN").font(mono(11.5))).clicked() {
                    reload = true;
                }
            });
            ui.horizontal_wrapped(|ui| {
                for label in ["1m", "5m", "15m", "30m", "1H"] {
                    if ui.selectable_label(pane.tf.eq_ignore_ascii_case(label), label).clicked() {
                        pane.tf = label.into();
                        reload = true;
                    }
                }
            });
            let show = if let Some(e) = &pane.input_error {
                crate::widgets::input_error_card(ui, e);
                false
            } else {
                crate::widgets::run_state(
                    ui,
                    pane.loading,
                    pane.error.as_deref(),
                    pane.chart.is_some(),
                    "Reading verified intraday bars…",
                    &mut reload,
                )
            };
            if show && let Some(chart) = &pane.chart {
                render_chart(ui, chart);
            }
        }
        if reload && let PaneKind::IntradayCompare(pane) = &mut self.panes[i].kind {
            crate::widgets::commit_anchor(&mut pane.symbol, &mut pane.ticker_draft);
            pane.request(id, &self.worker);
        }
        Vec::new()
    }
}

fn render_chart(ui: &mut Ui, chart: &CompareChart) {
    for note in &chart.notes {
        ui.label(RichText::new(note).font(ui_font(11.5)).color(amber()));
    }
    let height = 240.0;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let plot = Rect::from_min_max(rect.min + vec2(52.0, 8.0), rect.max - vec2(8.0, 22.0));
    let hover_line = {
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, theme::card());
        let (lo, hi) = y_bounds(chart);
        let span = (hi - lo).max(1e-6);
        let y_of = |pct: f64| -> f32 {
            let t = ((pct - lo) / span).clamp(0.0, 1.0) as f32;
            plot.bottom() - t * plot.height()
        };
        let n = chart.series.iter().find(|s| s.included).map(|s| s.points.len()).unwrap_or(0);
        let x_of = |i: usize| -> f32 {
            if n <= 1 {
                return plot.center().x;
            }
            plot.left() + i as f32 / (n - 1) as f32 * plot.width()
        };
        painter.hline(plot.x_range(), y_of(0.0), Stroke::new(1.0, theme::text().gamma_multiply(0.45)));
        painter.text(pos2(plot.left() - 6.0, y_of(0.0)), Align2::RIGHT_CENTER, "0", mono(10.0), muted());
        for pct in [lo, hi] {
            let y = y_of(pct);
            painter.hline(plot.x_range(), y, Stroke::new(1.0, theme::grid()));
            painter.text(pos2(plot.left() - 6.0, y), Align2::RIGHT_CENTER, format!("{pct:+.2}%"), mono(10.0), muted());
        }
        if n > 0 {
            for (i, label_at) in [0, n / 2, n - 1].into_iter().enumerate() {
                let idx = label_at.min(n - 1);
                let ts = chart.series.iter().find(|s| s.included).and_then(|s| s.points.get(idx)).map(|p| p.ts);
                if let Some(ts) = ts {
                    let x = x_of(idx);
                    painter.vline(x, plot.y_range(), Stroke::new(1.0, theme::grid()));
                    let align = match i {
                        0 => Align2::LEFT_TOP,
                        2 => Align2::RIGHT_TOP,
                        _ => Align2::CENTER_TOP,
                    };
                    painter.text(
                        pos2(x, plot.bottom() + 4.0),
                        align,
                        intraday_compare::clock_label(ts),
                        mono(10.0),
                        muted(),
                    );
                }
            }
        }
        for (i, series) in chart.series.iter().enumerate() {
            if !series.included {
                continue;
            }
            let color = series_color(i, series, &chart.anchor);
            let mut run: Vec<egui::Pos2> = Vec::new();
            let flush = |painter: &egui::Painter, run: &mut Vec<egui::Pos2>| {
                if run.len() >= 2 {
                    painter.line(run.clone(), Stroke::new(1.6, color));
                } else if let Some(p) = run.first() {
                    painter.circle_filled(*p, 2.4, color);
                }
                run.clear();
            };
            for (k, point) in series.points.iter().enumerate() {
                if point.pct.is_finite() {
                    run.push(pos2(x_of(k), y_of(point.pct)));
                } else {
                    flush(painter, &mut run);
                }
            }
            flush(painter, &mut run);
        }
        if let Some(pos) = response.hover_pos().filter(|p| plot.contains(*p))
            && n > 1
        {
            let t = ((pos.x - plot.left()) / plot.width()).clamp(0.0, 1.0);
            let idx = (t * (n - 1) as f32).round() as usize;
            let x = x_of(idx);
            painter.vline(x, plot.y_range(), Stroke::new(1.0, theme::border()));
            if let Some(ts) = chart.series.iter().find(|s| s.included).and_then(|s| s.points.get(idx)).map(|p| p.ts) {
                let mut bits = vec![intraday_compare::clock_label(ts)];
                for series in chart.series.iter().filter(|s| s.included) {
                    match series.points.get(idx).map(|p| p.pct) {
                        Some(pct) if pct.is_finite() => bits.push(format!("{} {pct:+.2}%", series.ticker)),
                        _ => bits.push(format!("{} gap", series.ticker)),
                    }
                }
                bits.join("   ")
            } else {
                String::new()
            }
        } else {
            String::new()
        }
    };
    if !hover_line.is_empty() {
        ui.label(RichText::new(hover_line).font(mono(11.0)).color(text()));
    }
    ui.add_space(4.0);
    egui::Grid::new(("icmp", chart.anchor.as_str(), chart.date.to_string()))
        .striped(true)
        .num_columns(6)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for title in ["Name", "Open", "Last", "At anchor", "vs SPY", ""] {
                ui.label(RichText::new(title).font(mono(10.5)).color(muted()));
            }
            ui.end_row();
            for (i, series) in chart.series.iter().enumerate() {
                let color = series_color(i, series, &chart.anchor);
                ui.label(RichText::new(&series.ticker).font(mono(12.0)).color(if series.included {
                    color
                } else {
                    muted()
                }));
                ui.label(
                    RichText::new(series.session_open.map(price_text).unwrap_or_else(|| "—".into())).font(mono(12.0)),
                );
                ui.label(RichText::new(last_text(series)).font(mono(12.0)).color(text()));
                ui.label(RichText::new(at_anchor_text(chart, series)).font(mono(12.0)).color(text()));
                ui.label(RichText::new(vs_text(chart, series)).font(mono(12.0)).color(text()));
                ui.label(RichText::new(footnote(series)).font(ui_font(11.0)).color(muted()));
                ui.end_row();
            }
        });
}

fn y_bounds(chart: &CompareChart) -> (f64, f64) {
    let mut lo = 0.0_f64;
    let mut hi = 0.0_f64;
    for series in chart.series.iter().filter(|s| s.included) {
        for point in &series.points {
            if point.pct.is_finite() {
                lo = lo.min(point.pct);
                hi = hi.max(point.pct);
            }
        }
    }
    lo = lo.min(0.0);
    hi = hi.max(0.0);
    let span = (hi - lo).max(0.5);
    let pad = span * 0.1;
    (lo - pad, hi + pad)
}

fn series_color(i: usize, series: &SymbolSeries, anchor: &str) -> Color32 {
    if series.symbol == anchor {
        theme::accent()
    } else if series.ticker.eq_ignore_ascii_case("SPY") {
        amber()
    } else {
        theme::series(i)
    }
}

fn price_text(v: f64) -> String {
    if v.abs() >= 1.0 { format!("{v:.2}") } else { format!("{v:.4}") }
}

fn last_text(series: &SymbolSeries) -> String {
    match (series.last_pct, series.last_ts) {
        (Some(pct), Some(ts)) => format!("{pct:+.2}% {}", intraday_compare::clock_label(ts)),
        _ => "—".into(),
    }
}

fn at_anchor_text(chart: &CompareChart, series: &SymbolSeries) -> String {
    let Some(ts) = chart.anchor_last else { return "—".into() };
    match series.pct_at(ts) {
        Some(pct) => format!("{pct:+.2}%"),
        None if series.included => "gap".into(),
        None => "—".into(),
    }
}

fn vs_text(chart: &CompareChart, series: &SymbolSeries) -> String {
    if series.ticker.eq_ignore_ascii_case("SPY") {
        "benchmark".into()
    } else {
        chart.versus_spy(series).map(|v| format!("{v:+.2} pp")).unwrap_or_else(|| "—".into())
    }
}

fn footnote(series: &SymbolSeries) -> String {
    if !series.included {
        return series.reason.clone();
    }
    let mut parts = Vec::new();
    if series.gaps > 0 {
        parts.push(format!("{} missing", series.gaps));
    }
    if series.blanks > 0 {
        parts.push(format!("{} left blank", series.blanks));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::command::{Command, Function};
    use chrono::NaiveDate;
    use egui::{Color32, Rect};
    use tr_core::{Bar, Provenance, Timeframe};
    use tr_data::intraday_compare::{NamedBars, build_chart};

    #[test]
    fn icmp_commands_open_the_pane_and_request_a_compare() {
        let mut app = test_app();
        app.apply(Function::IntradayCompare { spec: "MSFT 15m".into() });
        let pane = app
            .panes
            .iter()
            .find_map(|p| match &p.kind {
                PaneKind::IntradayCompare(c) => Some(c),
                _ => None,
            })
            .expect("compare pane");
        assert_eq!(pane.peers, "MSFT 15m");
        assert_eq!(pane.tf, "5m");
        assert_eq!(
            crate::command::parse("ICMP"),
            Some(Command::Function(Function::IntradayCompare { spec: String::new() }))
        );
        assert_eq!(
            crate::command::parse("AAPL ICMP MSFT NVDA 1m"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::IntradayCompare { spec: "MSFT NVDA 1m".into() }
            ))
        );
        assert!(matches!(crate::command::parse("ICOMPARE"), Some(Command::Function(Function::IntradayCompare { .. }))));

        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = ComparePane::new("AAPL", "MSFT 15m");
        pane.request(7, &worker);
        match requests.try_recv().unwrap() {
            Request::IntradayCompare { pane: id, symbol, spec, timeframe, generation } => {
                assert_eq!(id, 7);
                assert_eq!(generation, 1);
                assert_eq!(symbol, "AAPL US Equity");
                assert_eq!(spec, "MSFT 15m");
                assert_eq!(timeframe, "5m");
            }
            _ => panic!("unexpected request"),
        }
    }

    fn real_chart() -> CompareChart {
        let date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let bar = |hour, min, open, close| {
            let ts = tr_core::ny::to_utc(date.and_hms_opt(hour, min, 0).unwrap());
            Bar::new(ts, open, open.max(close) + 0.1, open.min(close) - 0.1, close, 100.0)
                .with_provenance(Provenance::Real)
        };
        build_chart(
            "AAPL",
            Timeframe::M30,
            &[
                NamedBars { symbol: "AAPL".into(), bars: vec![bar(9, 30, 10.0, 10.2)], error: None },
                NamedBars { symbol: "SPY".into(), bars: vec![bar(9, 30, 20.0, 20.0)], error: None },
            ],
            "unit",
        )
        .unwrap()
    }

    fn render_compare(app: &mut TerminalApp) -> Vec<(String, Color32)> {
        let id = app.focus;
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 600.0))),
                ..Default::default()
            },
            |ui| {
                app.compare_pane(ui, id);
            },
        );
        out.textures_delta.clear();
        crate::widgets::painted_texts(&out)
    }

    #[test]
    fn reload_keeps_the_previous_snapshot_and_a_failed_refresh_labels_it() {
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = ComparePane::new("AAPL", "");
        pane.generation = 1;
        pane.accept(1, Ok(real_chart()));
        assert!(pane.chart.is_some());
        // Reload: the old chart stays while the new request is in flight.
        pane.request(7, &worker);
        assert!(pane.loading && pane.chart.is_some());
        assert!(requests.try_recv().is_ok());
        // A failed refresh keeps the old chart and records the error.
        pane.accept(pane.generation, Err("request timed out".into()));
        assert!(pane.chart.is_some() && pane.error.as_deref() == Some("request timed out"));

        // Rendered: banner over the retained chart, no jump to a bare LOADING card.
        let mut app = test_app();
        pane.error = None;
        pane.loading = true;
        app.open_panel(PaneKind::IntradayCompare(Box::new(pane)));
        let t = render_compare(&mut app);
        assert!(t.iter().any(|(s, _)| s == "Updating · showing previous snapshot"), "{t:?}");
        assert!(!t.iter().any(|(s, _)| s == "LOADING"));
        assert!(t.iter().any(|(s, _)| s == "Name"), "previous chart table must stay on screen");

        let PaneKind::IntradayCompare(p) = &mut app.panes.last_mut().unwrap().kind else { unreachable!() };
        p.loading = false;
        p.error = Some("request timed out".into());
        let t = render_compare(&mut app);
        assert!(t.iter().any(|(s, _)| s.starts_with("Previous snapshot · refresh failed")), "{t:?}");
        // Refused (unverified) data still clears the result: unavailable stays unavailable.
        let mut sample = real_chart();
        sample.provenance = Provenance::Sample;
        let PaneKind::IntradayCompare(p) = &mut app.panes.last_mut().unwrap().kind else { unreachable!() };
        p.generation = 5;
        p.accept(5, Ok(sample));
        assert!(p.chart.is_none() && p.error.is_some());
    }

    #[test]
    fn a_kept_result_never_shows_under_another_ticker() {
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = ComparePane::new("AAPL", "");
        pane.generation = 1;
        pane.accept(1, Ok(real_chart()));
        // Same-symbol reload keeps the snapshot and shows the previous-snapshot banner.
        pane.request(7, &worker);
        assert!(pane.chart.is_some() && requests.try_recv().is_ok());
        let mut app = test_app();
        app.open_panel(PaneKind::IntradayCompare(Box::new(pane)));
        let t = render_compare(&mut app);
        assert!(t.iter().any(|(s, _)| s == "Updating · showing previous snapshot"), "{t:?}");
        // Another ticker drops the AAPL result: a plain LOADING card, no stale numbers.
        let PaneKind::IntradayCompare(p) = &mut app.panes.last_mut().unwrap().kind else { unreachable!() };
        p.symbol = "MSFT".into();
        p.request(7, &worker);
        assert!(p.chart.is_none() && p.loading);
        let t = render_compare(&mut app);
        assert!(t.iter().any(|(s, _)| s == "LOADING"), "{t:?}");
        assert!(!t.iter().any(|(s, _)| s.contains("previous snapshot")), "{t:?}");
    }

    #[test]
    fn a_linked_symbol_change_is_not_undone_by_a_reload_commit() {
        let mut app = test_app();
        let mut pane = ComparePane::new("AAPL", "");
        pane.generation = 1;
        pane.accept(1, Ok(real_chart()));
        app.open_panel(PaneKind::IntradayCompare(Box::new(pane)));
        render_compare(&mut app);
        let PaneKind::IntradayCompare(p) = &mut app.panes.last_mut().unwrap().kind else { unreachable!() };
        assert_eq!(p.ticker_draft, "AAPL US Equity");
        // The link path rewrites the symbol; the unfocused box follows it.
        p.symbol = "MSFT US Equity".into();
        render_compare(&mut app);
        let PaneKind::IntradayCompare(p) = &mut app.panes.last_mut().unwrap().kind else { unreachable!() };
        assert_eq!(p.ticker_draft, "MSFT US Equity");
        crate::widgets::commit_anchor(&mut p.symbol, &mut p.ticker_draft);
        assert_eq!(p.symbol, "MSFT US Equity");
        // An uncommitted, blurred edit is discarded, not loaded by Reload.
        p.ticker_draft = "MSF".into();
        render_compare(&mut app);
        let PaneKind::IntradayCompare(p) = &mut app.panes.last_mut().unwrap().kind else { unreachable!() };
        crate::widgets::commit_anchor(&mut p.symbol, &mut p.ticker_draft);
        assert_eq!(p.symbol, "MSFT US Equity");
    }

    #[test]
    fn a_bar_size_or_peer_change_drops_the_kept_result() {
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = ComparePane::new("AAPL", "");
        pane.generation = 1;
        pane.accept(1, Ok(real_chart()));
        assert_eq!(pane.tf, "30m");
        pane.request(7, &worker);
        assert!(pane.chart.is_some(), "same spec keeps the snapshot");
        assert!(requests.try_recv().is_ok());
        pane.tf = "5m".into();
        pane.request(7, &worker);
        assert!(pane.chart.is_none(), "another bar size must not show the old chart");
        pane.accept(pane.generation, Ok(real_chart()));
        pane.tf = "30m".into();
        pane.peers = "MSFT".into();
        pane.request(7, &worker);
        assert!(pane.chart.is_none(), "another peer set must not show the old chart");
    }

    #[test]
    fn emptying_the_anchor_draft_keeps_the_loaded_pane() {
        let mut app = test_app();
        let mut pane = ComparePane::new("AAPL", "");
        pane.generation = 1;
        pane.accept(1, Ok(real_chart()));
        pane.ticker_draft.clear();
        app.open_panel(PaneKind::IntradayCompare(Box::new(pane)));
        let t = render_compare(&mut app);
        assert!(t.iter().any(|(s, _)| s == "Anchor"), "the field must stay, not flip to the chooser: {t:?}");
        // The draft only becomes the symbol when committed, and never when empty.
        let mut symbol = "AAPL US Equity".to_string();
        let mut draft = "  ".to_string();
        crate::widgets::commit_anchor(&mut symbol, &mut draft);
        assert_eq!(symbol, "AAPL US Equity");
        let mut draft = "msft".to_string();
        crate::widgets::commit_anchor(&mut symbol, &mut draft);
        assert_eq!(symbol, "MSFT US Equity");
    }

    #[test]
    fn compare_pane_rejects_sample_charts() {
        let date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let bar = |hour, min, open, close| {
            let ts = tr_core::ny::to_utc(date.and_hms_opt(hour, min, 0).unwrap());
            Bar::new(ts, open, open.max(close) + 0.1, open.min(close) - 0.1, close, 100.0)
                .with_provenance(Provenance::Real)
        };
        let mut chart = build_chart(
            "AAPL",
            Timeframe::M30,
            &[
                NamedBars { symbol: "AAPL".into(), bars: vec![bar(9, 30, 10.0, 10.2)], error: None },
                NamedBars { symbol: "SPY".into(), bars: vec![bar(9, 30, 20.0, 20.0)], error: None },
            ],
            "unit",
        )
        .unwrap();
        chart.provenance = Provenance::Sample;
        let mut pane = ComparePane::new("AAPL", "");
        pane.generation = 1;
        pane.accept(1, Ok(chart));
        assert!(pane.chart.is_none());
        assert!(pane.error.as_deref().unwrap().contains("SAMPLE"));
    }
}
