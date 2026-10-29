//! Session extreme (`XTRM`): when the high and the low print.
use super::{PaneKind, TerminalApp};
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use tr_data::cash_session::{letter_bar_token, window_count_token, without_letter_bar_tokens};
use tr_data::session_extreme::{self, ExtremePoint, SessionExtreme};

pub struct SessionExtremePane {
    pub symbol: String,
    pub spec: String,
    pub tf: String,
    pub generation: u64,
    pub extreme: Option<SessionExtreme>,
    pub loading: bool,
    pub error: Option<String>,
}

impl SessionExtremePane {
    pub fn new(symbol: impl Into<String>, spec: impl Into<String>) -> Self {
        let symbol = symbol.into();
        let symbol = if symbol.trim().is_empty() { symbol } else { tr_data::security(&symbol).key() };
        Self { symbol, spec: spec.into(), tf: "5m".into(), generation: 0, extreme: None, loading: false, error: None }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        let symbol = self.symbol.trim();
        if symbol.is_empty() {
            self.error = Some("XTRM needs a ticker (AAPL XTRM). Sample prices are never used.".into());
            self.loading = false;
            return;
        }
        if let Err(err) = session_extreme::parse_query(&self.spec, &self.tf) {
            self.error = Some(err);
            self.loading = false;
            return;
        }
        self.generation += 1;
        self.loading = true;
        self.error = None;
        self.extreme = None;
        worker.send(Request::SessionExtreme {
            pane,
            generation: self.generation,
            symbol: tr_data::security(symbol).key(),
            spec: self.spec.clone(),
            timeframe: self.tf.clone(),
        });
    }

    pub fn accept(&mut self, generation: u64, result: Result<SessionExtreme, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(extreme) if !extreme.provenance.is_verified() => {
                self.extreme = None;
                self.error = Some(format!(
                    "Session extreme refused: {}. Verified real data is required.",
                    extreme.provenance.label()
                ));
            }
            Ok(extreme) => {
                self.tf = extreme.timeframe.label().into();
                self.extreme = Some(extreme);
                self.error = None;
            }
            Err(err) => {
                self.extreme = None;
                self.error = Some(err);
            }
        }
    }
}

impl TerminalApp {
    pub(super) fn open_session_extreme(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        let mut pane = SessionExtremePane::new(symbol, spec);
        if pane.symbol.is_empty() {
            pane.error = Some("XTRM needs a ticker (AAPL XTRM). Sample prices are never used.".into());
        }
        let ticker = if pane.symbol.is_empty() { "High".into() } else { tr_data::security(&pane.symbol).ticker };
        self.status = format!("{ticker}: when the session high and low print");
        self.open_panel(PaneKind::SessionExtreme(Box::new(pane)));
    }

    pub(super) fn session_extreme_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("session extreme pane");
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        let mut reload = false;
        {
            let PaneKind::SessionExtreme(pane) = &mut self.panes[i].kind else { return vec![] };
            if !pane.loading && pane.extreme.is_none() && pane.error.is_none() && !pane.symbol.trim().is_empty() {
                pane.request(id, &self.worker);
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Session extreme").strong().color(text()));
                ui.label(RichText::new("when the high and low print").font(mono(11.0)).color(amber()));
                if ui.small_button("Reload").on_hover_text("Read verified regular-session bars again").clicked() {
                    reload = true;
                }
            });
            ui.label(
                RichText::new(
                    "The minute is the first regular print of the session high or low. A later tie does not move it. Only a complete session votes, because a missing bar might hold the extreme. Today's marker is the running high and low so far. The rate past the last print is not a forecast. A bare number is how many prior sessions to keep.",
                )
                .font(ui_font(11.5))
                .color(muted()),
            );
            if sample_offline {
                ui.label(
                    RichText::new(
                        "Sample / offline display mode does not feed this pane. The extreme never falls back to sample bars.",
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
                for (label, line) in [("10", "10"), ("20", "20"), ("40", "40"), ("60", "60")] {
                    if ui.selectable_label(window_count_token(&pane.spec) == Some(line), label).clicked() {
                        pane.spec = match letter_bar_token(&pane.spec) {
                            Some(tf) => format!("{line} {tf}"),
                            None => line.into(),
                        };
                        reload = true;
                    }
                }
                for label in ["1m", "5m", "15m", "30m", "1H"] {
                    if ui.selectable_label(pane.tf.eq_ignore_ascii_case(label), label).clicked() {
                        pane.tf = label.into();
                        pane.spec = without_letter_bar_tokens(&pane.spec);
                        reload = true;
                    }
                }
            });
            if let Some(err) = &pane.error {
                ui.label(RichText::new(err).font(mono(12.0)).color(theme::down()));
            } else if pane.loading {
                ui.label(RichText::new("Reading verified intraday bars…").font(mono(12.0)).color(muted()));
            }
            if let Some(extreme) = &pane.extreme {
                render_extreme(ui, extreme);
            }
        }
        if reload && let PaneKind::SessionExtreme(pane) = &mut self.panes[i].kind {
            pane.request(id, &self.worker);
        }
        Vec::new()
    }
}

fn render_extreme(ui: &mut Ui, extreme: &SessionExtreme) {
    ui.label(
        RichText::new(format!(
            "{} · {} · {} complete sessions · {} · {}",
            extreme.date,
            extreme.timeframe.label(),
            extreme.priors_used,
            extreme.provenance.label(),
            extreme.source
        ))
        .font(mono(11.5))
        .color(if extreme.provenance.is_verified() { muted() } else { theme::down() }),
    );
    ui.label(RichText::new(summary_line(extreme)).font(mono(12.0)).color(text()));
    for note in &extreme.notes {
        ui.label(RichText::new(note).font(ui_font(11.5)).color(amber()));
    }
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 240.0), Sense::hover());
    let plot = Rect::from_min_max(rect.min + vec2(48.0, 8.0), rect.max - vec2(8.0, 22.0));
    let hover_line = {
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, theme::card());
        let y_of = |rate: f64| -> f32 {
            let t = rate.clamp(0.0, 1.0) as f32;
            plot.bottom() - t * plot.height()
        };
        let n = extreme.points.len();
        let x_of = |i: usize| -> f32 {
            if n <= 1 {
                return plot.center().x;
            }
            plot.left() + i as f32 / (n - 1) as f32 * plot.width()
        };
        for rate in [0.0, 1.0] {
            let y = y_of(rate);
            painter.hline(plot.x_range(), y, Stroke::new(1.0, theme::grid()));
            painter.text(
                pos2(plot.left() - 6.0, y),
                Align2::RIGHT_CENTER,
                format!("{:.0}%", rate * 100.0),
                mono(10.0),
                muted(),
            );
        }
        if n > 0 {
            for (i, idx) in [0, n / 2, n - 1].into_iter().enumerate() {
                let point = &extreme.points[idx];
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
                    session_extreme::clock_label(point.ts),
                    mono(10.0),
                    muted(),
                );
            }
        }
        draw_optional(painter, extreme, &x_of, &y_of, |point| point.low_in, amber(), 1.6);
        draw_optional(painter, extreme, &x_of, &y_of, |point| point.high_in, theme::accent(), 2.0);
        mark(painter, extreme, &x_of, plot, extreme.today_low_minute, amber());
        mark(painter, extreme, &x_of, plot, extreme.today_high_minute, theme::accent());
        if let Some(pos) = response.hover_pos().filter(|pos| plot.contains(*pos))
            && n > 1
        {
            let t = ((pos.x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
            let idx = (t * (n - 1) as f32).round() as usize;
            let point = &extreme.points[idx.min(n - 1)];
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
        ui.label(RichText::new("high already in").font(ui_font(11.0)).color(muted()));
        ui.label(RichText::new("—").color(amber()));
        ui.label(RichText::new("low already in").font(ui_font(11.0)).color(muted()));
    });
}

fn mark(
    painter: &egui::Painter,
    extreme: &SessionExtreme,
    x_of: &impl Fn(usize) -> f32,
    plot: Rect,
    minute: Option<u32>,
    color: Color32,
) {
    let Some(minute) = minute else { return };
    let Some(index) = extreme.points.iter().position(|point| point.minute == minute) else { return };
    painter.vline(x_of(index), plot.y_range(), Stroke::new(1.2, color));
}

fn draw_optional(
    painter: &egui::Painter,
    extreme: &SessionExtreme,
    x_of: &impl Fn(usize) -> f32,
    y_of: &impl Fn(f64) -> f32,
    pick: impl Fn(&ExtremePoint) -> Option<f64>,
    color: Color32,
    width: f32,
) {
    let mut run: Vec<egui::Pos2> = Vec::new();
    let flush = |painter: &egui::Painter, run: &mut Vec<egui::Pos2>| {
        if run.len() >= 2 {
            painter.line(run.clone(), Stroke::new(width, color));
        } else if let Some(point) = run.first() {
            painter.circle_filled(*point, 2.2, color);
        }
        run.clear();
    };
    for (i, point) in extreme.points.iter().enumerate() {
        if let Some(value) = pick(point) {
            run.push(pos2(x_of(i), y_of(value)));
        } else {
            flush(painter, &mut run);
        }
    }
    flush(painter, &mut run);
}

fn hover_text(point: &ExtremePoint) -> String {
    let high = point.high_in.map(rate_pct).unwrap_or_else(|| "—".into());
    let low = point.low_in.map(rate_pct).unwrap_or_else(|| "—".into());
    let today_high = point.today_high.map(session_extreme::minute_label).unwrap_or_else(|| "—".into());
    let today_low = point.today_low.map(session_extreme::minute_label).unwrap_or_else(|| "—".into());
    format!(
        "{}   high in {high}   low in {low}   running high {today_high}   running low {today_low}   n={}",
        session_extreme::clock_label(point.ts),
        point.samples
    )
}

fn summary_line(extreme: &SessionExtreme) -> String {
    let high = match (extreme.today_high_px, extreme.today_high_minute) {
        (Some(px), Some(minute)) => format!("{px:.2} at {}", session_extreme::minute_label(minute)),
        _ => "—".into(),
    };
    let low = match (extreme.today_low_px, extreme.today_low_minute) {
        (Some(px), Some(minute)) => format!("{px:.2} at {}", session_extreme::minute_label(minute)),
        _ => "—".into(),
    };
    let high_rate = extreme.high_in_at_last.map(rate_pct).unwrap_or_else(|| "blank".into());
    let low_rate = extreme.low_in_at_last.map(rate_pct).unwrap_or_else(|| "blank".into());
    let median = match (extreme.median_high_minute, extreme.median_low_minute) {
        (Some(high), Some(low)) => format!(
            "median high {} · median low {}",
            session_extreme::minute_label(high),
            session_extreme::minute_label(low)
        ),
        _ => "median blank".into(),
    };
    format!(
        "Running high {high} · running low {low} · by the last print high {high_rate} and low {low_rate} · {median}"
    )
}

fn rate_pct(value: f64) -> String {
    format!("{:.0}%", value * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::command::{Command, Function};
    use chrono::NaiveDate;
    use tr_core::{Bar, Provenance, Timeframe};
    use tr_data::intraday::regular_opens;
    use tr_data::session_extreme::{ExtremeQuery, build_extreme};

    #[test]
    fn hilo_commands_open_the_pane_and_request_an_extreme() {
        let mut app = test_app();
        app.apply(Function::SessionExtreme { spec: "20 15m".into() });
        let pane = app
            .panes
            .iter()
            .find_map(|pane| match &pane.kind {
                PaneKind::SessionExtreme(pane) => Some(pane),
                _ => None,
            })
            .expect("session extreme pane");
        assert_eq!(pane.spec, "20 15m");
        assert_eq!(pane.tf, "5m");
        assert_eq!(
            crate::command::parse("XTRM"),
            Some(Command::Function(Function::SessionExtreme { spec: String::new() }))
        );
        assert_eq!(
            crate::command::parse("AAPL XTRM 20 15m"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::SessionExtreme { spec: "20 15m".into() }))
        );
        assert!(matches!(crate::command::parse("XTRM"), Some(Command::Function(Function::SessionExtreme { .. }))));
        assert!(matches!(crate::command::parse("EXTREME"), Some(Command::Function(Function::SessionExtreme { .. }))));

        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = SessionExtremePane::new("AAPL", "20 15m");
        pane.request(7, &worker);
        match requests.try_recv().unwrap() {
            Request::SessionExtreme { pane: id, symbol, spec, timeframe, generation } => {
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
    fn session_extreme_pane_rejects_sample_charts() {
        let mut date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let mut days = Vec::new();
        while days.len() < 2 {
            if tr_data::history::calendar::is_session(date) {
                days.push(date);
            }
            date += chrono::Duration::days(1);
        }
        let mut bars = Vec::new();
        for day in days {
            for ts in regular_opens(day, Timeframe::M30) {
                bars.push(Bar::new(ts, 100.0, 101.0, 99.0, 100.5, 1.0).with_provenance(Provenance::Real));
            }
        }
        let mut extreme =
            build_extreme("AAPL", &bars, &ExtremeQuery { window: 5, timeframe: Timeframe::M30 }, "unit").unwrap();
        extreme.provenance = Provenance::Sample;
        let mut pane = SessionExtremePane::new("AAPL", "5");
        pane.generation = 1;
        pane.accept(1, Ok(extreme));
        assert!(pane.extreme.is_none());
        assert!(pane.error.as_deref().unwrap().contains("SAMPLE"));
    }
}
