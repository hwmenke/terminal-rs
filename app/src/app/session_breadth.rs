//! Session breadth (`BRD`): share of a list at or above the open and VWAP.
use std::str::FromStr;

use super::{PaneKind, TerminalApp};
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{Align2, Color32, Rect, RichText, ScrollArea, Sense, Stroke, Ui, pos2, vec2};
use tr_core::Timeframe;
use tr_data::session_breadth::{self, Breadth, BreadthPoint, Member};

pub struct BreadthPane {
    pub spec: String,
    pub tf: String,
    pub generation: u64,
    pub chart: Option<Breadth>,
    pub loading: bool,
    pub error: Option<String>,
}

impl BreadthPane {
    pub fn new(spec: impl Into<String>) -> Self {
        Self { spec: spec.into(), tf: "5m".into(), generation: 0, chart: None, loading: false, error: None }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker, watchlist: &[String]) {
        if let Err(err) = session_breadth::parse_query(&self.spec, &self.tf, watchlist) {
            self.error = Some(err);
            self.loading = false;
            return;
        }
        self.generation += 1;
        self.loading = true;
        self.error = None;
        self.chart = None;
        worker.send(Request::Breadth {
            pane,
            generation: self.generation,
            spec: self.spec.clone(),
            timeframe: self.tf.clone(),
            symbols: watchlist.to_vec(),
        });
    }

    pub fn accept(&mut self, generation: u64, result: Result<Breadth, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(chart) if !chart.provenance.is_verified() => {
                self.chart = None;
                self.error = Some(format!(
                    "Session breadth refused: {}. Verified real data is required.",
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
    fn breadth_watchlist(&self) -> Vec<String> {
        self.watchlists
            .group(&self.watch_ui.selected_group)
            .map(|g| g.members.iter().map(|m| m.symbol.clone()).collect())
            .unwrap_or_default()
    }

    pub(super) fn open_breadth(&mut self, symbol: Option<&str>, spec: &str) {
        let spec = match symbol {
            Some(s) => {
                let ticker = tr_data::security(s).ticker;
                let already = spec.split_whitespace().any(|t| t.eq_ignore_ascii_case(&ticker));
                if already { spec.trim().to_string() } else { format!("{ticker} {spec}").trim().to_string() }
            }
            None => spec.to_string(),
        };
        let pane = BreadthPane::new(spec);
        self.status = "Session breadth: share of the list at or above the 09:30 open and session VWAP".into();
        self.open_panel(PaneKind::Breadth(Box::new(pane)));
    }

    pub(super) fn breadth_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("breadth pane");
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        let symbols = self.breadth_watchlist();
        let mut reload = false;
        {
            let PaneKind::Breadth(pane) = &mut self.panes[i].kind else { return vec![] };
            if !pane.loading && pane.chart.is_none() && pane.error.is_none() {
                pane.request(id, &self.worker, &symbols);
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Session breadth").strong().color(text()));
                ui.label(RichText::new("at or above open and VWAP").font(mono(11.0)).color(amber()));
                if ui.small_button("Reload").on_hover_text("Read verified regular-session bars again").clicked() {
                    reload = true;
                }
            });
            ui.label(
                RichText::new(
                    "Each minute is the share of names with one print whose close is at or above their own 09:30 open, and the share at or above their own session VWAP. The rate stays blank below five names. A hole does not vote. Leave tickers empty to use the selected watchlist. 5 is a five-minute bar, and 60 is one hour.",
                )
                .font(ui_font(11.5))
                .color(muted()),
            );
            if sample_offline {
                ui.label(
                    RichText::new(
                        "Sample / offline display mode does not feed this pane. The breadth never falls back to sample bars.",
                    )
                    .font(ui_font(12.0))
                    .color(theme::down()),
                );
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Tickers").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.spec).desired_width(220.0).hint_text("AAPL MSFT 15m"));
                ui.label(RichText::new("Bars").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.tf).desired_width(48.0).hint_text("5m"));
                if ui.button(RichText::new("RUN").font(mono(11.5))).clicked() {
                    reload = true;
                }
                if ui.button("Watchlist").on_hover_text("Drop typed tickers and read the selected watchlist").clicked()
                {
                    pane.spec = spec_bar_size(&pane.spec).unwrap_or("").into();
                    reload = true;
                }
            });
            ui.horizontal_wrapped(|ui| {
                for label in ["1m", "5m", "15m", "30m", "1H"] {
                    if ui.selectable_label(pane.tf.eq_ignore_ascii_case(label), label).clicked() {
                        pane.tf = label.into();
                        pane.spec = drop_bar_sizes(&pane.spec);
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
                render_breadth(ui, chart);
            }
        }
        if reload && let PaneKind::Breadth(pane) = &mut self.panes[i].kind {
            pane.request(id, &self.worker, &symbols);
        }
        Vec::new()
    }
}

fn render_breadth(ui: &mut Ui, chart: &Breadth) {
    ui.label(
        RichText::new(format!(
            "{} · {} · {} names in the rate · {} outside · {} · {}",
            chart.date,
            chart.timeframe.label(),
            chart.names_used,
            chart.names_skipped,
            chart.provenance.label(),
            chart.source
        ))
        .font(mono(11.5))
        .color(if chart.provenance.is_verified() { muted() } else { theme::down() }),
    );
    ui.label(RichText::new(summary_line(chart)).font(mono(12.0)).color(text()));
    for note in &chart.notes {
        ui.label(RichText::new(note).font(ui_font(11.5)).color(amber()));
    }
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 220.0), Sense::hover());
    let plot = Rect::from_min_max(rect.min + vec2(52.0, 8.0), rect.max - vec2(8.0, 22.0));
    let hover_line = {
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, theme::card());
        let y_of = |pct: f64| -> f32 {
            let t = (pct / 100.0).clamp(0.0, 1.0) as f32;
            plot.bottom() - t * plot.height()
        };
        let n = chart.points.len();
        let x_of = |i: usize| -> f32 {
            if n <= 1 {
                return plot.center().x;
            }
            plot.left() + i as f32 / (n - 1) as f32 * plot.width()
        };
        for pct in [0.0, 50.0, 100.0] {
            let y = y_of(pct);
            painter.hline(plot.x_range(), y, Stroke::new(1.0, theme::grid()));
            painter.text(pos2(plot.left() - 6.0, y), Align2::RIGHT_CENTER, format!("{pct:.0}%"), mono(10.0), muted());
        }
        if n > 0 {
            for (i, idx) in [0, n / 2, n - 1].into_iter().enumerate() {
                let point = &chart.points[idx];
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
                    session_breadth::clock_label(point.ts),
                    mono(10.0),
                    muted(),
                );
            }
        }
        draw_rate(painter, chart, &x_of, &y_of, |p| p.above_vwap, amber(), 1.8);
        draw_rate(painter, chart, &x_of, &y_of, |p| p.above_open, theme::accent(), 2.0);
        if let Some(pos) = response.hover_pos().filter(|p| plot.contains(*p))
            && n > 1
        {
            let t = ((pos.x - plot.left()) / plot.width()).clamp(0.0, 1.0);
            let idx = (t * (n - 1) as f32).round() as usize;
            let point = &chart.points[idx.min(n - 1)];
            painter.vline(x_of(idx), plot.y_range(), Stroke::new(1.0, theme::border()));
            hover_text(point)
        } else {
            String::new()
        }
    };
    if !hover_line.is_empty() {
        ui.label(RichText::new(hover_line).font(mono(11.0)).color(text()));
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new("—").color(theme::accent()));
        ui.label(RichText::new("at or above the open").font(ui_font(11.0)).color(muted()));
        ui.label(RichText::new("—").color(amber()));
        ui.label(RichText::new("at or above VWAP").font(ui_font(11.0)).color(muted()));
    });
    ScrollArea::vertical().id_salt("breadth-roster").max_height(200.0).auto_shrink([false, true]).show(ui, |ui| {
        for member in &chart.members {
            let color = if member.included { text() } else { theme::down() };
            ui.label(RichText::new(member_line(member)).font(mono(11.5)).color(color));
        }
    });
}

fn draw_rate(
    painter: &egui::Painter,
    chart: &Breadth,
    x_of: &impl Fn(usize) -> f32,
    y_of: &impl Fn(f64) -> f32,
    pick: impl Fn(&BreadthPoint) -> Option<f64>,
    color: Color32,
    width: f32,
) {
    let mut run: Vec<egui::Pos2> = Vec::new();
    let flush = |painter: &egui::Painter, run: &mut Vec<egui::Pos2>| {
        if run.len() >= 2 {
            painter.line(run.clone(), Stroke::new(width, color));
        } else if let Some(p) = run.first() {
            painter.circle_filled(*p, 2.2, color);
        }
        run.clear();
    };
    for (i, point) in chart.points.iter().enumerate() {
        if let Some(pct) = pick(point) {
            run.push(pos2(x_of(i), y_of(pct)));
        } else {
            flush(painter, &mut run);
        }
    }
    flush(painter, &mut run);
}

fn hover_text(point: &BreadthPoint) -> String {
    format!(
        "{}   open {}   VWAP {}",
        session_breadth::clock_label(point.ts),
        share(point.above_open, point.open_yes, point.open_votes),
        share(point.above_vwap, point.vwap_yes, point.vwap_votes),
    )
}

fn summary_line(chart: &Breadth) -> String {
    let last = chart.points.last();
    let when = chart.last_ts.map(session_breadth::clock_label).unwrap_or_else(|| "—".into());
    let open = last.map(|p| share(p.above_open, p.open_yes, p.open_votes)).unwrap_or_else(|| "—".into());
    let vwap = last.map(|p| share(p.above_vwap, p.vwap_yes, p.vwap_votes)).unwrap_or_else(|| "—".into());
    format!("At or above the open {open}   at or above VWAP {vwap}   {when}")
}

fn share(rate: Option<f64>, yes: usize, votes: usize) -> String {
    match rate {
        Some(v) => format!("{yes}/{votes} {v:.0}%"),
        None => format!("{yes}/{votes}"),
    }
}

fn member_line(member: &Member) -> String {
    if !member.included {
        return format!("{:<6}  {}", member.ticker, member.reason);
    }
    let vs = member.vs_open.map(|v| format!("{v:+.2}%")).unwrap_or_else(|| "—".into());
    let open = match member.above_open {
        Some(true) => "at/above open",
        Some(false) => "below open",
        None => "open —",
    };
    let vwap = match member.above_vwap {
        Some(true) => "at/above VWAP",
        Some(false) => "below VWAP",
        None => "VWAP —",
    };
    let when = member.last_ts.map(session_breadth::clock_label).unwrap_or_else(|| "—".into());
    format!("{:<6}  {vs:>8}  {open:<14}  {vwap:<14}  {when}", member.ticker)
}

fn spec_bar_size(spec: &str) -> Option<&str> {
    spec.split_whitespace().find(|token| Timeframe::from_str(token).is_ok())
}

fn drop_bar_sizes(spec: &str) -> String {
    spec.split_whitespace().filter(|token| Timeframe::from_str(token).is_err()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::command::{Command, Function};
    use chrono::NaiveDate;
    use tr_core::{Bar, Provenance, Timeframe};
    use tr_data::intraday::regular_opens;
    use tr_data::session_breadth::{BreadthQuery, NamedBars, build_breadth};

    #[test]
    fn breadth_commands_open_the_pane_and_request_the_list() {
        let mut app = test_app();
        app.open_breadth(Some("AAPL"), "MSFT 15m");
        let pane = app
            .panes
            .iter()
            .find_map(|p| match &p.kind {
                PaneKind::Breadth(p) => Some(p),
                _ => None,
            })
            .expect("breadth pane");
        assert_eq!(pane.spec, "AAPL MSFT 15m");
        assert_eq!(crate::command::parse("BRD"), Some(Command::Function(Function::Breadth { spec: String::new() })));
        assert_eq!(
            crate::command::parse("AAPL BRD MSFT NVDA"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Breadth { spec: "MSFT NVDA".into() }))
        );
        assert!(matches!(crate::command::parse("BREADTH"), Some(Command::Function(Function::Breadth { .. }))));
        assert!(matches!(crate::command::parse("SBREADTH"), Some(Command::Function(Function::Breadth { .. }))));

        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = BreadthPane::new("");
        pane.request(7, &worker, &["AAPL".into(), "MSFT".into()]);
        match requests.try_recv().unwrap() {
            Request::Breadth { pane: id, spec, timeframe, symbols, generation } => {
                assert_eq!(id, 7);
                assert_eq!(generation, 1);
                assert!(spec.is_empty());
                assert_eq!(timeframe, "5m");
                assert_eq!(symbols, vec!["AAPL".to_string(), "MSFT".to_string()]);
            }
            _ => panic!("unexpected request"),
        }
    }

    #[test]
    fn breadth_pane_rejects_sample_charts() {
        let date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let names = ["AAPL", "MSFT", "NVDA", "AMD", "QQQ"];
        let bars: Vec<Bar> = regular_opens(date, Timeframe::M30)
            .into_iter()
            .map(|ts| Bar::new(ts, 10.0, 10.2, 9.9, 10.1, 100.0).with_provenance(Provenance::Real))
            .collect();
        let rows: Vec<_> =
            names.iter().map(|s| NamedBars { symbol: (*s).into(), bars: bars.clone(), error: None }).collect();
        let query = BreadthQuery {
            symbols: names.iter().map(|s| tr_data::security(s).key()).collect(),
            timeframe: Timeframe::M30,
            notes: vec![],
        };
        let mut chart = build_breadth(&rows, &query, "unit").unwrap();
        chart.provenance = Provenance::Sample;
        let mut pane = BreadthPane::new("AAPL MSFT NVDA AMD QQQ");
        pane.generation = 1;
        pane.accept(1, Ok(chart));
        assert!(pane.chart.is_none());
        assert!(pane.error.as_deref().unwrap().contains("SAMPLE"));
    }
}
