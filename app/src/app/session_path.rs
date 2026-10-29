//! Typical session path (`SPATH`): today versus the mean of prior 09:30 opens.
use super::{PaneKind, TerminalApp};
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use tr_data::session_path::{self, PathPoint, SessionPath};

pub struct SessionPathPane {
    pub symbol: String,
    pub spec: String,
    pub tf: String,
    pub generation: u64,
    pub path: Option<SessionPath>,
    pub loading: bool,
    pub error: Option<String>,
}

impl SessionPathPane {
    pub fn new(symbol: impl Into<String>, spec: impl Into<String>) -> Self {
        let symbol = symbol.into();
        let symbol = if symbol.trim().is_empty() { symbol } else { tr_data::security(&symbol).key() };
        Self { symbol, spec: spec.into(), tf: "5m".into(), generation: 0, path: None, loading: false, error: None }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        let symbol = self.symbol.trim();
        if symbol.is_empty() {
            self.error = Some("SPATH needs a ticker (AAPL SPATH). Sample prices are never used.".into());
            self.loading = false;
            return;
        }
        if let Err(err) = session_path::parse_query(&self.spec, &self.tf) {
            self.error = Some(err);
            self.loading = false;
            return;
        }
        self.generation += 1;
        self.loading = true;
        self.error = None;
        self.path = None;
        worker.send(Request::SessionPath {
            pane,
            generation: self.generation,
            symbol: tr_data::security(symbol).key(),
            spec: self.spec.clone(),
            timeframe: self.tf.clone(),
        });
    }

    pub fn accept(&mut self, generation: u64, result: Result<SessionPath, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(path) if !path.provenance.is_verified() => {
                self.path = None;
                self.error =
                    Some(format!("Session path refused: {}. Verified real data is required.", path.provenance.label()));
            }
            Ok(path) => {
                self.tf = path.timeframe.label().into();
                self.path = Some(path);
                self.error = None;
            }
            Err(err) => {
                self.path = None;
                self.error = Some(err);
            }
        }
    }
}

impl TerminalApp {
    pub(super) fn open_session_path(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        let mut pane = SessionPathPane::new(symbol, spec);
        if pane.symbol.is_empty() {
            pane.error = Some("SPATH needs a ticker (AAPL SPATH). Sample prices are never used.".into());
        }
        let ticker = if pane.symbol.is_empty() { "Path".into() } else { tr_data::security(&pane.symbol).ticker };
        self.status = format!("{ticker}: today versus the typical path from prior 09:30 opens");
        self.open_panel(PaneKind::SessionPath(Box::new(pane)));
    }

    pub(super) fn session_path_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("session path pane");
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        let mut reload = false;
        {
            let PaneKind::SessionPath(pane) = &mut self.panes[i].kind else { return vec![] };
            if !pane.loading && pane.path.is_none() && pane.error.is_none() && !pane.symbol.trim().is_empty() {
                pane.request(id, &self.worker);
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Session path").strong().color(text()));
                ui.label(RichText::new("today vs prior sessions").font(mono(11.0)).color(amber()));
                if ui.small_button("Reload").on_hover_text("Read verified regular-session bars again").clicked() {
                    reload = true;
                }
            });
            ui.label(
                RichText::new(
                    "Today and each prior session are percent from their own 09:30 open. The mean uses earlier sessions only, and only minutes with at least five prints. A missing bucket does not vote. The line past the last print is not a forecast.",
                )
                .font(ui_font(11.5))
                .color(muted()),
            );
            if sample_offline {
                ui.label(
                    RichText::new(
                        "Sample / offline display mode does not feed this pane. The path never falls back to sample bars.",
                    )
                    .font(ui_font(12.0))
                    .color(theme::down()),
                );
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Ticker").color(muted()));
                let changed = ui
                    .add(egui::TextEdit::singleline(&mut pane.symbol).desired_width(140.0).hint_text("AAPL"))
                    .lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if changed {
                    reload = true;
                }
                ui.label(RichText::new("Sessions").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.spec).desired_width(88.0).hint_text("20 5m"));
                ui.label(RichText::new("Bars").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.tf).desired_width(48.0).hint_text("5m"));
                if ui.button(RichText::new("RUN").font(mono(11.5))).clicked() {
                    reload = true;
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (label, line) in [("5", "5"), ("10", "10"), ("20", "20"), ("40", "40")] {
                    if ui.selectable_label(spec_count(&pane.spec) == Some(line), label).clicked() {
                        pane.spec = match spec_bar_size(&pane.spec) {
                            Some(tf) => format!("{line} {tf}"),
                            None => line.into(),
                        };
                        reload = true;
                    }
                }
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
            if let Some(path) = &pane.path {
                render_path(ui, path);
            }
        }
        if reload && let PaneKind::SessionPath(pane) = &mut self.panes[i].kind {
            pane.request(id, &self.worker);
        }
        Vec::new()
    }
}

fn render_path(ui: &mut Ui, path: &SessionPath) {
    ui.label(
        RichText::new(format!(
            "{} · {} · {} prior sessions · {} · {}",
            path.date,
            path.timeframe.label(),
            path.priors_used,
            path.provenance.label(),
            path.source
        ))
        .font(mono(11.5))
        .color(if path.provenance.is_verified() { muted() } else { theme::down() }),
    );
    ui.label(RichText::new(summary_line(path)).font(mono(12.0)).color(text()));
    for note in &path.notes {
        ui.label(RichText::new(note).font(ui_font(11.5)).color(amber()));
    }
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 240.0), Sense::hover());
    let plot = Rect::from_min_max(rect.min + vec2(52.0, 8.0), rect.max - vec2(8.0, 22.0));
    let hover_line = {
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, theme::card());
        let (lo, hi) = y_bounds(path);
        let span = (hi - lo).max(1e-6);
        let y_of = |pct: f64| -> f32 {
            let t = ((pct - lo) / span).clamp(0.0, 1.0) as f32;
            plot.bottom() - t * plot.height()
        };
        let n = path.points.len();
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
            for (i, idx) in [0, n / 2, n - 1].into_iter().enumerate() {
                let point = &path.points[idx];
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
                    session_path::clock_label(point.ts),
                    mono(10.0),
                    muted(),
                );
            }
        }
        draw_optional(painter, path, &x_of, &y_of, |p| p.p25, amber().gamma_multiply(0.45), 1.0);
        draw_optional(painter, path, &x_of, &y_of, |p| p.p75, amber().gamma_multiply(0.45), 1.0);
        draw_optional(painter, path, &x_of, &y_of, |p| p.typical, amber(), 1.8);
        draw_optional(painter, path, &x_of, &y_of, |p| p.today, theme::accent(), 2.0);
        if let Some(pos) = response.hover_pos().filter(|p| plot.contains(*p))
            && n > 1
        {
            let t = ((pos.x - plot.left()) / plot.width()).clamp(0.0, 1.0);
            let idx = (t * (n - 1) as f32).round() as usize;
            let point = &path.points[idx.min(n - 1)];
            painter.vline(x_of(idx), plot.y_range(), Stroke::new(1.0, theme::border()));
            hover_text(point)
        } else {
            String::new()
        }
    };
    if !hover_line.is_empty() {
        ui.label(RichText::new(hover_line).font(mono(11.0)).color(text()));
    }
}

fn draw_optional(
    painter: &egui::Painter,
    path: &SessionPath,
    x_of: &impl Fn(usize) -> f32,
    y_of: &impl Fn(f64) -> f32,
    pick: impl Fn(&PathPoint) -> Option<f64>,
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
    for (i, point) in path.points.iter().enumerate() {
        if let Some(pct) = pick(point) {
            run.push(pos2(x_of(i), y_of(pct)));
        } else {
            flush(painter, &mut run);
        }
    }
    flush(painter, &mut run);
}

fn hover_text(point: &PathPoint) -> String {
    let today = point.today.map(|v| format!("{v:+.2}%")).unwrap_or_else(|| "gap".into());
    let typical = point.typical.map(|v| format!("{v:+.2}%")).unwrap_or_else(|| "—".into());
    let band = match (point.p25, point.p75) {
        (Some(a), Some(b)) => format!("{a:+.2}%..{b:+.2}%"),
        _ => "—".into(),
    };
    format!(
        "{}   today {today}   typical {typical}   band {band}   n={}",
        session_path::clock_label(point.ts),
        point.samples
    )
}

fn summary_line(path: &SessionPath) -> String {
    let when = path.today_last_ts.map(session_path::clock_label).unwrap_or_else(|| "—".into());
    let today = path.today_last.map(|v| format!("{v:+.2}%")).unwrap_or_else(|| "—".into());
    let typical = path.typical_at_last.map(|v| format!("{v:+.2}%")).unwrap_or_else(|| "—".into());
    let versus = path.versus_typical.map(|v| format!("{v:+.2} pp")).unwrap_or_else(|| "—".into());
    format!("Open {:.2}   last {today} at {when}   typical {typical}   vs typical {versus}", path.session_open)
}

fn spec_count(spec: &str) -> Option<&str> {
    spec.split_whitespace().find(|token| token.chars().all(|c| c.is_ascii_digit()))
}

fn spec_bar_size(spec: &str) -> Option<&str> {
    spec.split_whitespace()
        .find(|token| token.chars().any(|c| c.is_ascii_digit()) && token.chars().any(|c| c.is_ascii_alphabetic()))
}

fn drop_bar_sizes(spec: &str) -> String {
    spec.split_whitespace()
        .filter(|token| !(token.chars().any(|c| c.is_ascii_digit()) && token.chars().any(|c| c.is_ascii_alphabetic())))
        .collect::<Vec<_>>()
        .join(" ")
}

fn y_bounds(path: &SessionPath) -> (f64, f64) {
    let mut lo = 0.0_f64;
    let mut hi = 0.0_f64;
    for point in &path.points {
        for value in [point.today, point.typical, point.p25, point.p75].into_iter().flatten() {
            lo = lo.min(value);
            hi = hi.max(value);
        }
    }
    lo = lo.min(0.0);
    hi = hi.max(0.0);
    let pad = (hi - lo).max(0.5) * 0.1;
    (lo - pad, hi + pad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::command::{Command, Function};
    use chrono::NaiveDate;
    use tr_core::{Bar, Provenance, Timeframe};
    use tr_data::intraday::regular_opens;
    use tr_data::session_path::{PathQuery, build_path};

    #[test]
    fn spath_commands_open_the_pane_and_request_a_path() {
        let mut app = test_app();
        app.apply(Function::SessionPath { spec: "20 15m".into() });
        let pane = app
            .panes
            .iter()
            .find_map(|p| match &p.kind {
                PaneKind::SessionPath(p) => Some(p),
                _ => None,
            })
            .expect("session path pane");
        assert_eq!(pane.spec, "20 15m");
        assert_eq!(pane.tf, "5m");
        assert_eq!(
            crate::command::parse("SPATH"),
            Some(Command::Function(Function::SessionPath { spec: String::new() }))
        );
        assert_eq!(
            crate::command::parse("AAPL SPATH 20 15m"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::SessionPath { spec: "20 15m".into() }))
        );
        assert!(matches!(crate::command::parse("SESSIONPATH"), Some(Command::Function(Function::SessionPath { .. }))));

        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = SessionPathPane::new("AAPL", "20 15m");
        pane.request(7, &worker);
        match requests.try_recv().unwrap() {
            Request::SessionPath { pane: id, symbol, spec, timeframe, generation } => {
                assert_eq!(id, 7);
                assert_eq!(generation, 1);
                assert_eq!(symbol, "AAPL US Equity");
                assert_eq!(spec, "20 15m");
                assert_eq!(timeframe, "5m");
            }
            _ => panic!("unexpected request"),
        }
    }

    #[test]
    fn session_path_pane_rejects_sample_charts() {
        let mut date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let mut days = Vec::new();
        while days.len() < 6 {
            if tr_data::history::calendar::is_session(date) {
                days.push(date);
            }
            date += chrono::Duration::days(1);
        }
        let mut bars = Vec::new();
        for day in days {
            for ts in regular_opens(day, Timeframe::M30) {
                bars.push(Bar::new(ts, 10.0, 10.2, 9.9, 10.1, 100.0).with_provenance(Provenance::Real));
            }
        }
        let mut path = build_path("AAPL", &bars, &PathQuery { window: 5, timeframe: Timeframe::M30 }, "unit").unwrap();
        path.provenance = Provenance::Sample;
        let mut pane = SessionPathPane::new("AAPL", "5");
        pane.generation = 1;
        pane.accept(1, Ok(path));
        assert!(pane.path.is_none());
        assert!(pane.error.as_deref().unwrap().contains("SAMPLE"));
    }
}
