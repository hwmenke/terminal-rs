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
}

impl ComparePane {
    pub fn new(symbol: impl Into<String>, peers: impl Into<String>) -> Self {
        let symbol = symbol.into();
        let symbol = if symbol.trim().is_empty() { symbol } else { tr_data::security(&symbol).key() };
        Self { symbol, peers: peers.into(), tf: "5m".into(), generation: 0, chart: None, loading: false, error: None }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        let symbol = self.symbol.trim();
        if symbol.is_empty() {
            self.error = Some("ICMP needs a ticker (AAPL ICMP MSFT). Sample prices are never used.".into());
            self.loading = false;
            return;
        }
        if let Err(err) = intraday_compare::parse_query(symbol, &self.peers, &self.tf) {
            self.error = Some(err);
            self.loading = false;
            return;
        }
        self.generation += 1;
        self.loading = true;
        self.error = None;
        self.chart = None;
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
            Err(err) => {
                self.chart = None;
                self.error = Some(err);
            }
        }
    }
}

impl TerminalApp {
    pub(super) fn open_intraday_compare(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        let mut pane = ComparePane::new(symbol, spec);
        if pane.symbol.is_empty() {
            pane.error = Some("ICMP needs a ticker (AAPL ICMP MSFT). Sample prices are never used.".into());
        }
        let ticker = if pane.symbol.is_empty() { "Compare".into() } else { tr_data::security(&pane.symbol).ticker };
        self.status = format!("{ticker}: intraday compare, percent from each 09:30 open");
        self.open_panel(PaneKind::IntradayCompare(Box::new(pane)));
    }

    pub(super) fn compare_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("compare pane");
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        let mut reload = false;
        {
            let PaneKind::IntradayCompare(pane) = &mut self.panes[i].kind else { return vec![] };
            if !pane.loading && pane.chart.is_none() && pane.error.is_none() && !pane.symbol.trim().is_empty() {
                pane.request(id, &self.worker);
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Session compare").strong().color(text()));
                ui.label(RichText::new("percent from the 09:30 open").font(mono(11.0)).color(amber()));
                if ui.small_button("Reload").on_hover_text("Read verified regular-session bars again").clicked() {
                    reload = true;
                }
            });
            ui.label(
                RichText::new(
                    "Each line is that name's close divided by its own 09:30 open. The zero line is the open. SPY is on the same clock. A missing bucket is a gap. Premarket and after-hours prints stay off the chart.",
                )
                .font(ui_font(11.5))
                .color(muted()),
            );
            if sample_offline {
                ui.label(
                    RichText::new(
                        "Sample / offline display mode does not feed this pane. The compare never falls back to sample bars.",
                    )
                    .font(ui_font(12.0))
                    .color(theme::down()),
                );
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Anchor").color(muted()));
                let changed = ui
                    .add(egui::TextEdit::singleline(&mut pane.symbol).desired_width(140.0).hint_text("AAPL"))
                    .lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if changed {
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
            if let Some(err) = &pane.error {
                ui.label(RichText::new(err).font(mono(12.0)).color(theme::down()));
            } else if pane.loading {
                ui.label(RichText::new("Reading verified intraday bars…").font(mono(12.0)).color(muted()));
            }
            if let Some(chart) = &pane.chart {
                render_chart(ui, chart);
            }
        }
        if reload && let PaneKind::IntradayCompare(pane) = &mut self.panes[i].kind {
            pane.request(id, &self.worker);
        }
        Vec::new()
    }
}

fn render_chart(ui: &mut Ui, chart: &CompareChart) {
    ui.label(
        RichText::new(format!(
            "{} · {} · {} · {}",
            chart.date,
            chart.timeframe.label(),
            chart.provenance.label(),
            chart.source
        ))
        .font(mono(11.5))
        .color(if chart.provenance.is_verified() { muted() } else { theme::down() }),
    );
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
