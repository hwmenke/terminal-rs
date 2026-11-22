//! Volume pace (`VPACE`): today's cumulative volume versus prior sessions.
use super::{PaneKind, TerminalApp};
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use tr_data::cash_session::{letter_bar_token, window_count_token, without_letter_bar_tokens};
use tr_data::volume_pace::{self, PacePoint, VolumePace};

pub struct VolumePacePane {
    pub symbol: String,
    pub spec: String,
    pub tf: String,
    pub generation: u64,
    pub pace: Option<VolumePace>,
    pub loading: bool,
    pub error: Option<String>,
    /// A typo in the request (bar size, sessions…): a "check the inputs" card, not a failed run.
    pub input_error: Option<String>,
    /// Text-box buffer of the ticker chooser; the real `symbol` is only set on commit. Not persisted.
    pub ticker_draft: String,
}

impl VolumePacePane {
    pub fn new(symbol: impl Into<String>, spec: impl Into<String>) -> Self {
        let symbol = symbol.into();
        let symbol = if symbol.trim().is_empty() { symbol } else { tr_data::security(&symbol).key() };
        Self { symbol, spec: spec.into(), tf: "5m".into(), generation: 0, pace: None, loading: false, error: None, input_error: None, ticker_draft: String::new() }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        let symbol = self.symbol.trim();
        if symbol.is_empty() {
            // Needs input, not an error: the pane shows a ticker chooser.
            self.loading = false;
            return;
        }
        if let Err(err) = volume_pace::parse_query(&self.spec, &self.tf) {
            self.input_error = Some(err);
            self.loading = false;
            return;
        }
        self.input_error = None;
        // A kept snapshot belongs to one ticker: never show it under another name.
        // Nor under another bar size.
        let tf = volume_pace::parse_query(&self.spec, &self.tf).ok().map(|q| q.timeframe);
        if self.pace.as_ref().is_some_and(|p| !crate::widgets::same_security(&p.symbol, symbol) || Some(p.timeframe) != tf) {
            self.pace = None;
        }
        self.generation += 1;
        self.loading = true;
        self.error = None;
        worker.send(Request::VolumePace {
            pane,
            generation: self.generation,
            symbol: tr_data::security(symbol).key(),
            spec: self.spec.clone(),
            timeframe: self.tf.clone(),
        });
    }

    pub fn accept(&mut self, generation: u64, result: Result<VolumePace, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(pace) if !pace.provenance.is_verified() => {
                self.pace = None;
                self.error =
                    Some(format!("Volume pace refused: {}. Verified real data is required.", pace.provenance.label()));
            }
            Ok(pace) => {
                self.tf = pace.timeframe.label().into();
                self.pace = Some(pace);
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
    pub(super) fn open_volume_pace(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        // No focused ticker is a setup state (a ticker chooser), not an error.
        let pane = VolumePacePane::new(symbol, spec);
        let ticker = if pane.symbol.is_empty() { "Pace".into() } else { tr_data::security(&pane.symbol).ticker };
        self.status = format!("{ticker}: cumulative volume versus the usual pace from prior sessions");
        self.open_panel(PaneKind::VolumePace(Box::new(pane)));
    }

    pub(super) fn volume_pace_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("volume pace pane");
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        let mut reload = false;
        let watchlist = self.watchlist.clone();
        {
            let PaneKind::VolumePace(pane) = &mut self.panes[i].kind else { return vec![] };
            if !pane.loading && pane.pace.is_none() && pane.error.is_none() && pane.input_error.is_none() && !pane.symbol.trim().is_empty() {
                pane.request(id, &self.worker);
            }
            let has_symbol = !pane.symbol.trim().is_empty();
            let context = if let Some(p) = &pane.pace {
                p.ticker.clone()
            } else if has_symbol {
                tr_data::security(&pane.symbol).ticker
            } else {
                "cumulative shares vs prior".into()
            };
            // Intraday results carry no structured receipt, so the chip is the honest compact legacy line.
            let legacy = pane.pace.as_ref().map(|p| {
                format!("{} · {} · {} · {}", p.date, p.timeframe.label(), p.provenance.label(), p.source)
            });
            let source = legacy.as_deref().map(|l| crate::widgets::HeaderSource { lead: "", parts: &[], legacy: l });
            crate::widgets::pane_header(ui, "Volume pace", &context, source, |ui| {
                if has_symbol
                    && crate::widgets::header_button(ui, "Reload", "Read verified regular-session bars again", false)
                        .clicked()
                {
                    reload = true;
                }
            });
            if !has_symbol {
                let what = "Track today's cumulative volume against the usual pace of prior sessions, on real intraday bars.";
                if let Some(key) = crate::widgets::needs_ticker(ui, what, &mut pane.ticker_draft, &watchlist) {
                    pane.symbol = key;
                    pane.ticker_draft.clear();
                    pane.request(id, &self.worker);
                }
                return Vec::new();
            }
            crate::widgets::methodology(
                ui,
                (id, "vpace-method"),
                "Today's running total is the sum of one print per minute from 09:30. The mean uses earlier sessions only, and only minutes where five of them still have a complete total. A hole ends that session's cumulative. The line past the last print is not a forecast. A bare number is how many prior sessions to keep.",
            );
            if sample_offline {
                crate::widgets::chip(
                    ui,
                    crate::widgets::ChipKind::Neutral,
                    "Sample display mode: this pane needs live research data and never uses sample bars",
                );
            }
            ui.horizontal_wrapped(|ui| {
                if crate::widgets::anchor_field(ui, "Ticker", &mut pane.ticker_draft, &pane.symbol) {
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
            let show = if let Some(e) = &pane.input_error {
                crate::widgets::input_error_card(ui, e);
                false
            } else {
                crate::widgets::run_state(
                    ui,
                    pane.loading,
                    pane.error.as_deref(),
                    pane.pace.is_some(),
                    "Reading verified intraday bars…",
                    &mut reload,
                )
            };
            if show && let Some(pace) = &pane.pace {
                render_pace(ui, pace);
            }
        }
        if reload && let PaneKind::VolumePace(pane) = &mut self.panes[i].kind {
            crate::widgets::commit_anchor(&mut pane.symbol, &mut pane.ticker_draft);
            pane.request(id, &self.worker);
        }
        Vec::new()
    }
}

fn render_pace(ui: &mut Ui, pace: &VolumePace) {
    ui.label(RichText::new(format!("{} prior sessions", pace.priors_used)).font(mono(11.0)).color(muted()));
    ui.label(RichText::new(summary_line(pace)).font(mono(12.0)).color(text()));
    for note in &pace.notes {
        ui.label(RichText::new(note).font(ui_font(11.5)).color(amber()));
    }
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 240.0), Sense::hover());
    let plot = Rect::from_min_max(rect.min + vec2(58.0, 8.0), rect.max - vec2(8.0, 22.0));
    let hover_line = {
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, theme::card());
        let (lo, hi) = y_bounds(pace);
        let span = (hi - lo).max(1e-6);
        let y_of = |vol: f64| -> f32 {
            let t = ((vol - lo) / span).clamp(0.0, 1.0) as f32;
            plot.bottom() - t * plot.height()
        };
        let n = pace.points.len();
        let x_of = |i: usize| -> f32 {
            if n <= 1 {
                return plot.center().x;
            }
            plot.left() + i as f32 / (n - 1) as f32 * plot.width()
        };
        for vol in [lo, hi] {
            let y = y_of(vol);
            painter.hline(plot.x_range(), y, Stroke::new(1.0, theme::grid()));
            painter.text(pos2(plot.left() - 6.0, y), Align2::RIGHT_CENTER, compact(vol), mono(10.0), muted());
        }
        if n > 0 {
            for (i, idx) in [0, n / 2, n - 1].into_iter().enumerate() {
                let point = &pace.points[idx];
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
                    volume_pace::clock_label(point.ts),
                    mono(10.0),
                    muted(),
                );
            }
        }
        draw_optional(painter, pace, &x_of, &y_of, |p| p.p25, amber().gamma_multiply(0.45), 1.0);
        draw_optional(painter, pace, &x_of, &y_of, |p| p.p75, amber().gamma_multiply(0.45), 1.0);
        draw_optional(painter, pace, &x_of, &y_of, |p| p.typical, amber(), 1.8);
        draw_optional(painter, pace, &x_of, &y_of, |p| p.today, theme::accent(), 2.0);
        if let Some(pos) = response.hover_pos().filter(|p| plot.contains(*p))
            && n > 1
        {
            let t = ((pos.x - plot.left()) / plot.width()).clamp(0.0, 1.0);
            let idx = (t * (n - 1) as f32).round() as usize;
            let point = &pace.points[idx.min(n - 1)];
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
        ui.label(RichText::new("today").font(ui_font(11.0)).color(muted()));
        ui.label(RichText::new("—").color(amber()));
        ui.label(RichText::new("typical and 25th–75th").font(ui_font(11.0)).color(muted()));
    });
}

fn draw_optional(
    painter: &egui::Painter,
    pace: &VolumePace,
    x_of: &impl Fn(usize) -> f32,
    y_of: &impl Fn(f64) -> f32,
    pick: impl Fn(&PacePoint) -> Option<f64>,
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
    for (i, point) in pace.points.iter().enumerate() {
        if let Some(vol) = pick(point) {
            run.push(pos2(x_of(i), y_of(vol)));
        } else {
            flush(painter, &mut run);
        }
    }
    flush(painter, &mut run);
}

fn hover_text(point: &PacePoint) -> String {
    let today = point.today.map(compact).unwrap_or_else(|| "gap".into());
    let typical = point.typical.map(compact).unwrap_or_else(|| "—".into());
    let band = match (point.p25, point.p75) {
        (Some(a), Some(b)) => format!("{}..{}", compact(a), compact(b)),
        _ => "—".into(),
    };
    let pace = match (point.today, point.typical) {
        (Some(now), Some(typ)) if typ > 0.0 => format!("{:.2}×", now / typ),
        _ => "—".into(),
    };
    format!(
        "{}   today {today}   typical {typical}   band {band}   pace {pace}   n={}",
        volume_pace::clock_label(point.ts),
        point.samples
    )
}

fn summary_line(pace: &VolumePace) -> String {
    let when = pace.today_last_ts.map(volume_pace::clock_label).unwrap_or_else(|| "—".into());
    let today = pace.today_last.map(compact).unwrap_or_else(|| "—".into());
    let typical = pace.typical_at_last.map(compact).unwrap_or_else(|| "—".into());
    let multiple = pace.pace.map(|v| format!("{v:.2}×")).unwrap_or_else(|| "—".into());
    format!("Cumulative {today} at {when}   typical {typical}   pace {multiple}")
}

fn compact(volume: f64) -> String {
    let abs = volume.abs();
    if abs >= 1.0e9 {
        format!("{:.2}B", volume / 1.0e9)
    } else if abs >= 1.0e6 {
        format!("{:.2}M", volume / 1.0e6)
    } else if abs >= 1.0e3 {
        format!("{:.1}k", volume / 1.0e3)
    } else {
        format!("{volume:.0}")
    }
}

fn y_bounds(pace: &VolumePace) -> (f64, f64) {
    let mut hi = 0.0_f64;
    for point in &pace.points {
        for value in [point.today, point.typical, point.p25, point.p75].into_iter().flatten() {
            hi = hi.max(value);
        }
    }
    (0.0, hi.max(1.0) * 1.08)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::command::{Command, Function};
    use chrono::NaiveDate;
    use tr_core::{Bar, Provenance, Timeframe};
    use tr_data::intraday::regular_opens;
    use tr_data::volume_pace::{PaceQuery, build_pace};

    #[test]
    fn vpace_commands_open_the_pane_and_request_a_pace() {
        let mut app = test_app();
        app.apply(Function::VolumePace { spec: "20 15m".into() });
        let pane = app
            .panes
            .iter()
            .find_map(|p| match &p.kind {
                PaneKind::VolumePace(p) => Some(p),
                _ => None,
            })
            .expect("volume pace pane");
        assert_eq!(pane.spec, "20 15m");
        assert_eq!(pane.tf, "5m");
        assert_eq!(
            crate::command::parse("VPACE"),
            Some(Command::Function(Function::VolumePace { spec: String::new() }))
        );
        assert_eq!(
            crate::command::parse("AAPL VPACE 20 15m"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::VolumePace { spec: "20 15m".into() }))
        );
        assert!(matches!(crate::command::parse("VOLPACE"), Some(Command::Function(Function::VolumePace { .. }))));
        assert!(matches!(crate::command::parse("CUMVOL"), Some(Command::Function(Function::VolumePace { .. }))));

        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = VolumePacePane::new("AAPL", "20 15m");
        pane.request(7, &worker);
        match requests.try_recv().unwrap() {
            Request::VolumePace { pane: id, symbol, spec, timeframe, generation } => {
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
    fn volume_pace_pane_rejects_sample_charts() {
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
        let mut pace = build_pace("AAPL", &bars, &PaceQuery { window: 5, timeframe: Timeframe::M30 }, "unit").unwrap();
        pace.provenance = Provenance::Sample;
        let mut pane = VolumePacePane::new("AAPL", "5");
        pane.generation = 1;
        pane.accept(1, Ok(pace));
        assert!(pane.pace.is_none());
        assert!(pane.error.as_deref().unwrap().contains("SAMPLE"));
    }
}
