//! CLOCK pane. Inference belongs to the worker/data report, never to paint.
use super::{PaneKind, TerminalApp};
use crate::{
    congress_view::Action,
    dock::PaneId,
    theme::{self, mono, muted, text},
    widgets,
    worker::Request,
};
use egui::{Align2, Color32, Key, Modifiers, Rect, RichText, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};
use tr_data::session_clock::{self, ClockCell, MIN_SAMPLES, ROWS, SessionClock};

pub struct SessionClockPane {
    pub symbol: String,
    pub spec: String,
    pub tf: String,
    pub generation: u64,
    pub clock: Option<SessionClock>,
    pub loading: bool,
    pub error: Option<String>,
    requested: Option<std::time::Instant>,
}

impl SessionClockPane {
    pub fn new(symbol: impl Into<String>, spec: impl Into<String>) -> Self {
        let symbol = symbol.into();
        let symbol = if symbol.trim().is_empty() { symbol } else { tr_data::security(&symbol).key() };
        Self {
            symbol,
            spec: spec.into(),
            tf: "30m".into(),
            generation: 0,
            clock: None,
            loading: false,
            error: None,
            requested: None,
        }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        // Invalidate in-flight results even when the edited request is refused.
        self.generation += 1;
        self.clock = None;
        self.loading = false;
        self.requested = Some(std::time::Instant::now());
        if self.symbol.trim().is_empty() {
            self.error = Some("CLOCK needs a ticker. Sample prices are never used.".into());
            return;
        }
        if let Err(err) = session_clock::parse_query(&self.spec, &self.tf) {
            self.error = Some(err);
            return;
        }
        self.loading = true;
        self.error = None;
        worker.send(Request::SessionClock {
            pane,
            generation: self.generation,
            symbol: tr_data::security(&self.symbol).key(),
            spec: self.spec.clone(),
            timeframe: self.tf.clone(),
        });
    }

    pub fn accept(&mut self, generation: u64, result: Result<SessionClock, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(clock) if clock.provenance.is_verified() => {
                self.tf = clock.timeframe.label().into();
                self.clock = Some(clock);
                self.error = None;
            }
            Ok(_) => {
                self.clock = None;
                self.error = Some("Session Clock refused: verified real data is required.".into());
            }
            Err(err) => {
                self.clock = None;
                self.error = Some(err);
            }
        }
    }
}

impl TerminalApp {
    pub(super) fn open_session_clock(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        self.status = "Session Clock: weekday by regular-session bucket returns".into();
        self.open_panel(PaneKind::SessionClock(Box::new(SessionClockPane::new(symbol, spec))));
    }

    pub(super) fn session_clock_periodic(&mut self, visible: &[usize]) {
        let now = chrono::Utc::now().timestamp();
        for &i in visible {
            let p = &mut self.panes[i];
            if let PaneKind::SessionClock(clock) = &mut p.kind {
                let initial = clock.requested.is_none();
                let date_changed = clock.clock.as_ref().is_some_and(|r| r.date != tr_core::ny::civil(now).date());
                let refresh = clock.clock.is_some()
                    && clock.requested.is_some_and(|t| t.elapsed().as_secs() >= 60)
                    && (date_changed
                        || tr_data::intraday::in_regular_session(now)
                        || tr_data::intraday::in_regular_session(now - 60));
                if !clock.loading && (initial || refresh) {
                    clock.request(p.id, &self.worker);
                }
            }
        }
    }

    pub(super) fn clock_keys(&mut self, ctx: &egui::Context) -> bool {
        if !ctx.input(|i| i.key_pressed(Key::R) && !i.modifiers.any()) {
            return false;
        }
        let Some(i) = self.idx(self.focus) else {
            return false;
        };
        let id = self.panes[i].id;
        let PaneKind::SessionClock(pane) = &mut self.panes[i].kind else {
            return false;
        };
        pane.request(id, &self.worker);
        ctx.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::R);
            i.events.retain(|e| !matches!(e, egui::Event::Text(t) if t.eq_ignore_ascii_case("r")));
        });
        true
    }

    pub(super) fn session_clock_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("session clock pane");
        let sample = self.provider.eq_ignore_ascii_case("sample");
        let PaneKind::SessionClock(pane) = &mut self.panes[i].kind else {
            return vec![];
        };
        let mut reload = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Session Clock").strong().color(text()));
            if ui.small_button("Reload · R").clicked() {
                reload = true;
            }
        });
        let refused =
            sample || pane.error.is_some() || pane.clock.as_ref().is_some_and(|r| !r.provenance.is_verified());
        if refused {
            let reason = if sample {
                "Sample / offline mode does not feed Session Clock. Verified real data is required."
            } else {
                pane.error.as_deref().unwrap_or("Verified real data is required.")
            };
            reload |= refusal_card(ui, reason);
            ui.label(RichText::new("Choose a ticker with the CLOCK command.").color(muted()));
            if !sample && ui.button("Reset buckets").clicked() {
                pane.spec.clear();
                pane.tf = "30m".into();
                reload = true;
            }
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::TextEdit::singleline(&mut pane.symbol).desired_width(130.0).hint_text("AAPL"));
                for label in ["15m", "30m", "1h"] {
                    let selected = session_clock::parse_query(&pane.spec, &pane.tf)
                        .is_ok_and(|q| q.timeframe.label().eq_ignore_ascii_case(label));
                    if ui.selectable_label(selected, label).clicked() {
                        pane.spec = label.into();
                        reload = true;
                    }
                }
                if ui.button("RUN").clicked() {
                    reload = true;
                }
            });
            if pane.loading {
                ui.label(RichText::new("Reading verified cash-session bars…").color(muted()));
            }
            if let Some(clock) = &pane.clock {
                render_clock(ui, clock, chrono::Utc::now().timestamp());
            }
        }
        if reload {
            pane.request(id, &self.worker);
        }
        Vec::new()
    }
}

fn refusal_card(ui: &mut Ui, reason: &str) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 160.0), Sense::hover());
    // Provider errors can contain retry counts and status codes. Refusals print
    // no digits; prices or statistical placeholders never appear here.
    let reason: String = reason.chars().filter(|c| !c.is_ascii_digit()).collect();
    widgets::unavailable_card(ui, rect, &tr_data::FeedState::Unsupported { reason })
}

fn render_clock(ui: &mut Ui, clock: &SessionClock, now: i64) {
    // Defense in depth for callers outside the pane's response gate.
    if !clock.provenance.is_verified() {
        refusal_card(ui, "Verified real data is required.");
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!(
                "{} of {} pass · joint p {}",
                clock.passing,
                clock.tested,
                probability(clock.joint_p)
            ))
            .font(mono(12.0))
            .color(text()),
        );
        if clock.stale {
            widgets::chip(ui, widgets::ChipKind::Caution, "STALE")
                .on_hover_text("Input trails the cash-session grid or has no fresh observation clock. R retries.");
        }
    });
    if !clock.joint_passes() {
        ui.label(RichText::new("NO CLOCK EFFECT DETECTED").font(mono(12.0)).color(theme::amber()));
    }
    ui.label(
        RichText::new(format!(
            "{} · {} · {} · {} prior sessions · {} · {}",
            clock.ticker,
            clock.date,
            clock.timeframe.label(),
            clock.sessions,
            clock.provenance.label(),
            clock.source
        ))
        .font(mono(10.0))
        .color(muted()),
    );
    widgets::heat_key(ui, 10.0, "bp");
    let columns = clock.minutes.len();
    let width = ui.available_width().max(46.0 + columns as f32 * 38.0);
    egui::ScrollArea::horizontal().id_salt("clock-grid").show(ui, |ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(width, 254.0), Sense::hover());
        let col_width = (width - 46.0) / columns as f32;
        let now_col = session_clock::now_column(clock.timeframe, now);
        let painter = ui.painter();
        for (col, minute) in clock.minutes.iter().enumerate() {
            let x = rect.left() + 46.0 + col as f32 * col_width;
            if now_col == Some(col) {
                let highlight = Rect::from_min_size(pos2(x, rect.top()), vec2(col_width, 252.0));
                painter.rect_stroke(highlight, 2.0, Stroke::new(1.5, theme::amber()), StrokeKind::Inside);
            }
            painter.text(
                pos2(x + col_width / 2.0, rect.top() + 12.0),
                Align2::CENTER_CENTER,
                session_clock::minute_label(*minute),
                mono(9.0),
                if now_col == Some(col) { theme::amber() } else { muted() },
            );
        }
        for (row, label) in ROWS.iter().chain(std::iter::once(&"TODAY")).enumerate() {
            let y = rect.top() + 28.0 + row as f32 * 31.5;
            painter.text(pos2(rect.left(), y + 14.0), Align2::LEFT_CENTER, *label, mono(10.0), muted());
            for col in 0..columns {
                let r = Rect::from_min_size(
                    pos2(rect.left() + 46.0 + col as f32 * col_width + 2.0, y),
                    vec2(col_width - 4.0, 28.0),
                );
                if row < 6 {
                    let cell = &clock.cells[row][col];
                    paint_cell(painter, r, cell.mean, cell.passes(clock.joint_p), cell.n < MIN_SAMPLES);
                    ui.interact(r, ui.id().with(("clock", row, col)), Sense::hover()).on_hover_text(hover_cell(cell));
                } else {
                    // TODAY is descriptive only: it has no p/q and never earns a significance fill.
                    paint_cell(
                        painter,
                        r,
                        if clock.date == tr_core::ny::civil(now).date() { clock.today[col] } else { None },
                        false,
                        false,
                    );
                    ui.interact(r, ui.id().with(("today", col)), Sense::hover())
                        .on_hover_text("TODAY · closed buckets only · excluded from every test");
                }
            }
        }
    });
    ui.label(
        RichText::new("Fill: q ≤ .10 AND joint p ≤ .10 · corner tick: LOW N (<8) · TODAY: closed only")
            .font(mono(10.0))
            .color(muted()),
    );
    ui.label(
        RichText::new(
            "Descriptive history, not a forecast. Hover for sign-test p, BH q, t and dates. Holes stay blank.",
        )
        .font(mono(10.0))
        .color(muted()),
    );
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
}

fn paint_cell(p: &egui::Painter, rect: Rect, mean: Option<f64>, fill: bool, low_n: bool) {
    if fill && let Some(mean) = mean {
        p.rect_filled(rect, 2.0, widgets::heat_ramp(mean, 10.0));
    }
    p.rect_stroke(rect, 2.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
    let label = mean
        .map(|m| if m.abs() < 1000.0 { format!("{m:+.1}") } else { format!("{:+.0}k", m / 1000.0) })
        .unwrap_or_else(|| "—".into());
    p.text(rect.center(), Align2::CENTER_CENTER, label, mono(10.0), if fill { Color32::WHITE } else { muted() });
    if low_n {
        p.add(Shape::line(
            vec![rect.left_top() + vec2(1.0, 6.0), rect.left_top() + vec2(1.0, 1.0), rect.left_top() + vec2(6.0, 1.0)],
            Stroke::new(1.0, theme::amber()),
        ));
    }
}

fn probability(p: Option<f64>) -> String {
    p.map(|p| format!("{p:.3}")).unwrap_or_else(|| "—".into())
}
fn value(v: Option<f64>) -> String {
    v.map(|v| format!("{v:+.2}")).unwrap_or_else(|| "—".into())
}
fn hover_cell(c: &ClockCell) -> String {
    format!(
        "n {}{}\nmean {} bp · median {} bp\nhit rate {}\nsign-test p {} · BH q {}\nt-stat {} (hover only)\n{} → {}",
        c.n,
        if c.n < MIN_SAMPLES { " · LOW N" } else { "" },
        value(c.mean),
        value(c.median),
        c.hit_rate.map(|v| format!("{:.1}%", v * 100.0)).unwrap_or_else(|| "—".into()),
        probability(c.p),
        probability(c.q),
        value(c.t_stat),
        c.first.map(|d| d.to_string()).unwrap_or_else(|| "—".into()),
        c.last.map(|d| d.to_string()).unwrap_or_else(|| "—".into())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::command::{Command, Function};
    use tr_core::{Bar, Provenance, Timeframe};

    fn fixture() -> SessionClock {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let ts = tr_core::ny::to_utc(date.and_hms_opt(9, 30, 0).unwrap());
        let bars = vec![Bar::new(ts, 100.0, 101.0, 99.0, 100.1, 1.0).with_provenance(Provenance::Real)];
        session_clock::build_clock(
            "AAPL",
            &bars,
            &session_clock::ClockQuery { timeframe: Timeframe::M30 },
            "unit",
            ts + 86400,
            Some(ts + 86400),
        )
        .unwrap()
    }
    fn texts(output: &egui::FullOutput) -> Vec<String> {
        output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                Shape::Text(t) => Some(t.galley.job.text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn clock_commands_parse_open_and_request_the_pane() {
        assert_eq!(
            crate::command::parse("CLOCK"),
            Some(Command::Function(Function::SessionClock { spec: String::new() }))
        );
        assert_eq!(
            crate::command::parse("aapl clock"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::SessionClock { spec: String::new() }))
        );
        assert_eq!(
            crate::command::parse("AAPL CLOCK 1h"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::SessionClock { spec: "1h".into() }))
        );
        assert_eq!(crate::command::parse("NEW CLOCK"), Some(Command::Function(Function::NewPanel("CLOCK".into()))));
        let mut app = test_app();
        app.open_session_clock(Some("AAPL"), "1h");
        let PaneKind::SessionClock(pane) = &app.panes[app.idx(app.focus).unwrap()].kind else {
            panic!("CLOCK pane");
        };
        assert_eq!(pane.symbol, "AAPL US Equity");
        assert_eq!(pane.spec, "1h");
        assert_eq!(pane.tf, "30m");
        assert!(pane.loading);
        assert_eq!(app.focused_symbol().as_deref(), Some("AAPL US Equity"));
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = SessionClockPane::new("AAPL", "1h");
        pane.request(7, &worker);
        assert!(
            matches!(requests.try_recv().unwrap(), Request::SessionClock { pane: 7, generation: 1, symbol, spec, timeframe } if symbol == "AAPL US Equity" && spec == "1h" && timeframe == "30m")
        );
        pane.request(7, &worker);
        pane.accept(1, Ok(fixture()));
        assert!(pane.loading && pane.clock.is_none(), "late response cannot replace retry");
        pane.spec = "1d".into();
        pane.request(7, &worker);
        pane.accept(2, Ok(fixture()));
        assert!(!pane.loading && pane.clock.is_none() && pane.error.is_some());
    }

    #[test]
    fn clock_new_panel_and_layout_restore_preserve_bucket_settings() {
        let mut app = test_app();
        app.apply(Function::NewPanel("CLOCK 1h".into()));
        assert!(app.panes.iter().any(|p| matches!(&p.kind, PaneKind::SessionClock(c) if c.spec == "1h")));
        let settings = crate::layouts::PaneSettings::SessionClock {
            symbol: "AAPL US Equity".into(),
            spec: "1h".into(),
            tf: "30m".into(),
        };
        let json = serde_json::to_string(&settings).unwrap();
        let back: crate::layouts::PaneSettings = serde_json::from_str(&json).unwrap();
        let pane = PaneKind::from_settings(back);
        assert_eq!(pane.settings(), settings);
        let old: crate::layouts::PaneSettings =
            serde_json::from_str(r#"{"SessionClock":{"symbol":"AAPL US Equity"}}"#).unwrap();
        let PaneKind::SessionClock(p) = PaneKind::from_settings(old) else {
            panic!("clock");
        };
        assert_eq!(p.tf, "30m");
    }

    #[test]
    fn clock_sample_and_unverified_paint_no_digits() {
        let mut app = test_app();
        app.open_session_clock(Some("AAPL"), "");
        let id = app.focus;
        let i = app.idx(id).unwrap();
        let PaneKind::SessionClock(pane) = &mut app.panes[i].kind else {
            panic!("clock");
        };
        let mut report = fixture();
        report.provenance = Provenance::Sample;
        pane.accept(pane.generation, Ok(report));
        assert!(pane.clock.is_none());
        pane.symbol = "ABC123 US Equity".into();
        let ctx = egui::Context::default();
        for provider in ["real fixture", "sample"] {
            app.provider = provider.into();
            let mut out = ctx.run_ui(Default::default(), |ui| {
                app.session_clock_pane(ui, id);
            });
            out.textures_delta.clear();
            let text = texts(&out).join(" ");
            assert!(text.contains("NO REAL DATA"), "{text}");
            assert!(!text.chars().any(|c| c.is_ascii_digit()), "{text}");
        }
        let mut report = fixture();
        report.provenance = Provenance::Missing;
        let mut out = ctx.run_ui(Default::default(), |ui| render_clock(ui, &report, report.last_ts));
        out.textures_delta.clear();
        let text = texts(&out).join(" ");
        assert!(text.contains("NO REAL DATA") && !text.chars().any(|c| c.is_ascii_digit()));
    }

    #[test]
    fn clock_paint_fills_only_when_both_tests_pass_and_stays_readable() {
        let ctx = egui::Context::default();
        let mut r = fixture();
        for row in &mut r.cells {
            row.fill(ClockCell::default());
        }
        r.cells[5][1] = ClockCell { n: 20, mean: Some(20.0), p: Some(0.001), q: Some(0.02), ..Default::default() };
        r.tested = 1;
        r.stale = true;
        for (joint_p, q, fills) in
            [(Some(0.2), Some(0.02), 0), (None, Some(0.02), 0), (Some(0.01), Some(0.2), 0), (Some(0.01), Some(0.02), 1)]
        {
            r.joint_p = joint_p;
            r.cells[5][1].q = q;
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(600.0, 600.0))),
                    ..Default::default()
                },
                |ui| {
                    render_clock(ui, &r, r.last_ts);
                },
            );
            out.textures_delta.clear();
            let filled = out.shapes.iter().filter(|s| matches!(&s.shape, Shape::Rect(rect) if rect.fill == widgets::heat_ramp(20.0, 10.0) && rect.rect.height() == 28.0)).count();
            assert_eq!(filled, fills);
            let text = texts(&out);
            assert!(text.iter().any(|t| t == "STALE"));
            assert_eq!(text.iter().any(|t| t == "NO CLOCK EFFECT DETECTED"), !r.joint_passes());
            let labels: Vec<_> = out
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    Shape::Text(t) if t.galley.job.text == "09:30" || t.galley.job.text == "15:30" => Some(t.pos.x),
                    _ => None,
                })
                .collect();
            assert_eq!(labels.len(), 2);
            assert!(labels.iter().all(|x| (0.0..600.0).contains(x)));
        }
    }

    #[test]
    fn clock_retry_key_and_paint_do_not_trigger_implicit_fetches() {
        let mut app = test_app();
        app.open_session_clock(Some("AAPL"), "");
        app.provider = "real fixture".into();
        let id = app.focus;
        let i = app.idx(id).unwrap();
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        app.worker = worker;
        let PaneKind::SessionClock(pane) = &mut app.panes[i].kind else {
            panic!("clock");
        };
        pane.accept(pane.generation, Ok(fixture()));
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            app.session_clock_pane(ui, id);
        });
        out.textures_delta.clear();
        assert!(requests.try_recv().is_err(), "painting a ready report never requests data");
        let mut out = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: Key::R,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                assert!(app.clock_keys(ui.ctx()));
            },
        );
        out.textures_delta.clear();
        assert!(matches!(requests.try_recv().unwrap(), Request::SessionClock { pane, .. } if pane == id));
    }
}
