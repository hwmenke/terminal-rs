//! Gap fill (`GFILL`): today's open versus the prior cash close.
use super::{PaneKind, TerminalApp};
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use tr_data::cash_session::{letter_bar_token, window_count_token, without_letter_bar_tokens};
use tr_data::gap_fill::{self, GapDirection, GapFill, GapPoint};

pub struct GapFillPane {
    pub symbol: String,
    pub spec: String,
    pub tf: String,
    pub generation: u64,
    pub gap: Option<GapFill>,
    pub loading: bool,
    pub error: Option<String>,
    /// A typo in the request (bar size, sessions…): a "check the inputs" card, not a failed run.
    pub input_error: Option<String>,
    /// Text-box buffer of the ticker chooser; the real `symbol` is only set on commit. Not persisted.
    pub ticker_draft: String,
}

impl GapFillPane {
    pub fn new(symbol: impl Into<String>, spec: impl Into<String>) -> Self {
        let symbol = symbol.into();
        let symbol = if symbol.trim().is_empty() { symbol } else { tr_data::security(&symbol).key() };
        Self { symbol, spec: spec.into(), tf: "5m".into(), generation: 0, gap: None, loading: false, error: None, input_error: None, ticker_draft: String::new() }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        let symbol = self.symbol.trim();
        if symbol.is_empty() {
            // Needs input, not an error: the pane shows a ticker chooser.
            self.loading = false;
            return;
        }
        if let Err(err) = gap_fill::parse_query(&self.spec, &self.tf) {
            self.input_error = Some(err);
            self.loading = false;
            return;
        }
        self.input_error = None;
        // A kept snapshot belongs to one ticker: never show it under another name.
        // Nor under another bar size.
        let tf = gap_fill::parse_query(&self.spec, &self.tf).ok().map(|q| q.timeframe);
        if self.gap.as_ref().is_some_and(|g| !crate::widgets::same_security(&g.symbol, symbol) || Some(g.timeframe) != tf) {
            self.gap = None;
        }
        self.generation += 1;
        self.loading = true;
        self.error = None;
        worker.send(Request::GapFill {
            pane,
            generation: self.generation,
            symbol: tr_data::security(symbol).key(),
            spec: self.spec.clone(),
            timeframe: self.tf.clone(),
        });
    }

    pub fn accept(&mut self, generation: u64, result: Result<GapFill, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(gap) if !gap.provenance.is_verified() => {
                self.gap = None;
                self.error =
                    Some(format!("Gap fill refused: {}. Verified real data is required.", gap.provenance.label()));
            }
            Ok(gap) => {
                self.tf = gap.timeframe.label().into();
                self.gap = Some(gap);
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
    pub(super) fn open_gap_fill(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        // No focused ticker is a setup state (a ticker chooser), not an error.
        let pane = GapFillPane::new(symbol, spec);
        let ticker = if pane.symbol.is_empty() { "Gap".into() } else { tr_data::security(&pane.symbol).ticker };
        self.status = format!("{ticker}: open versus the prior cash close, and how often that gap had filled");
        self.open_panel(PaneKind::GapFill(Box::new(pane)));
    }

    pub(super) fn gap_fill_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("gap fill pane");
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        let mut reload = false;
        let watchlist = self.watchlist.clone();
        {
            let PaneKind::GapFill(pane) = &mut self.panes[i].kind else { return vec![] };
            if !pane.loading && pane.gap.is_none() && pane.error.is_none() && pane.input_error.is_none() && !pane.symbol.trim().is_empty() {
                pane.request(id, &self.worker);
            }
            let has_symbol = !pane.symbol.trim().is_empty();
            let context = if let Some(g) = &pane.gap {
                g.ticker.clone()
            } else if has_symbol {
                tr_data::security(&pane.symbol).ticker
            } else { "open vs prior close".into() };
            // Intraday results carry no structured receipt, so the chip is the honest compact legacy line.
            let legacy = pane.gap.as_ref().map(|g| {
                format!("{} · {} · {} · {}", g.date, g.timeframe.label(), g.provenance.label(), g.source)
            });
            let source = legacy.as_deref().map(|l| crate::widgets::HeaderSource { lead: "", parts: &[], legacy: l });
            crate::widgets::pane_header(ui, "Gap fill", &context, source, |ui| {
                if has_symbol
                    && crate::widgets::header_button(ui, "Reload", "Read verified regular-session bars again", false)
                        .clicked()
                {
                    reload = true;
                }
            });
            if !has_symbol {
                let what = "See how today's 09:30 open compares with the prior cash close and how often that gap filled, on real intraday bars.";
                if let Some(key) = crate::widgets::needs_ticker(ui, what, &mut pane.ticker_draft, &watchlist) {
                    pane.symbol = key;
                    pane.ticker_draft.clear();
                    pane.request(id, &self.worker);
                }
                return Vec::new();
            }
            crate::widgets::methodology(
                ui,
                (id, "gfill-method"),
                "The gap is the 09:30 open versus the prior session's cash close. A fill is a trade through that close, and it stays filled. The rate is earlier same-direction gaps only. A hole ends the known path. The rate past the last print is not a forecast. A bare number is how many prior sessions to keep.",
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
                ui.add(egui::TextEdit::singleline(&mut pane.spec).desired_width(88.0).hint_text("40 5m"));
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
            let show = if let Some(e) = &pane.input_error {
                crate::widgets::input_error_card(ui, e);
                false
            } else {
                crate::widgets::run_state(
                    ui,
                    pane.loading,
                    pane.error.as_deref(),
                    pane.gap.is_some(),
                    "Reading verified intraday bars…",
                    &mut reload,
                )
            };
            if show && let Some(gap) = &pane.gap {
                render_gap(ui, gap);
            }
        }
        if reload && let PaneKind::GapFill(pane) = &mut self.panes[i].kind {
            crate::widgets::commit_anchor(&mut pane.symbol, &mut pane.ticker_draft);
            pane.request(id, &self.worker);
        }
        Vec::new()
    }
}

fn render_gap(ui: &mut Ui, gap: &GapFill) {
    ui.label(
        RichText::new(format!("{} prior sessions · {} same direction", gap.priors_used, gap.same_direction))
            .font(mono(11.0))
            .color(muted()),
    );
    ui.label(RichText::new(summary_line(gap)).font(mono(12.0)).color(text()));
    for note in &gap.notes {
        ui.label(RichText::new(note).font(ui_font(11.5)).color(amber()));
    }
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 300.0), Sense::hover());
    let price = Rect::from_min_max(rect.min + vec2(58.0, 8.0), pos2(rect.right() - 8.0, rect.bottom() - 118.0));
    let rate = Rect::from_min_max(pos2(price.left(), rect.bottom() - 96.0), pos2(price.right(), rect.bottom() - 22.0));
    let hover_line = {
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, theme::card());
        let (lo, hi) = y_bounds(gap);
        let span = (hi - lo).max(1e-6);
        let y_of = |value: f64| -> f32 {
            let t = ((value - lo) / span).clamp(0.0, 1.0) as f32;
            price.bottom() - t * price.height()
        };
        let n = gap.points.len();
        let x_of = |i: usize| -> f32 {
            if n <= 1 {
                return price.center().x;
            }
            price.left() + i as f32 / (n - 1) as f32 * price.width()
        };
        for value in [lo, 0.0, hi] {
            let y = y_of(value);
            painter.hline(price.x_range(), y, Stroke::new(1.0, theme::grid()));
            painter.text(
                pos2(price.left() - 6.0, y),
                Align2::RIGHT_CENTER,
                format!("{value:+.1}%"),
                mono(10.0),
                muted(),
            );
        }
        if n > 0 {
            for (i, idx) in [0, n / 2, n - 1].into_iter().enumerate() {
                let point = &gap.points[idx];
                let x = x_of(idx);
                painter.vline(x, price.y_range(), Stroke::new(1.0, theme::grid()));
                let align = match i {
                    0 => Align2::LEFT_TOP,
                    2 => Align2::RIGHT_TOP,
                    _ => Align2::CENTER_TOP,
                };
                painter.text(
                    pos2(x, price.bottom() + 2.0),
                    align,
                    gap_fill::clock_label(point.ts),
                    mono(10.0),
                    muted(),
                );
            }
        }
        draw_optional(painter, gap, &x_of, &y_of, |point| point.typical, amber(), 1.8);
        draw_optional(painter, gap, &x_of, &y_of, |point| point.today, theme::accent(), 2.0);
        if let Some(index) =
            gap.fill_minute.and_then(|minute| gap.points.iter().position(|point| point.minute == minute))
        {
            painter.vline(x_of(index), price.y_range(), Stroke::new(1.0, theme::up()));
        }
        painter.text(
            pos2(rate.left(), rate.top() - 12.0),
            Align2::LEFT_CENTER,
            "Filled by this minute",
            mono(10.0),
            muted(),
        );
        let y_rate = |value: f64| -> f32 {
            let t = value.clamp(0.0, 1.0) as f32;
            rate.bottom() - t * rate.height()
        };
        for value in [0.0, 1.0] {
            let y = y_rate(value);
            painter.hline(rate.x_range(), y, Stroke::new(1.0, theme::grid()));
            painter.text(
                pos2(rate.left() - 6.0, y),
                Align2::RIGHT_CENTER,
                format!("{:.0}%", value * 100.0),
                mono(10.0),
                muted(),
            );
        }
        draw_optional(painter, gap, &x_of, &y_rate, |point| point.fill_rate, theme::up(), 1.8);
        if let Some(pos) = response.hover_pos().filter(|pos| price.contains(*pos) || rate.contains(*pos))
            && n > 1
        {
            let width = price.width().max(1.0);
            let t = ((pos.x - price.left()) / width).clamp(0.0, 1.0);
            let idx = (t * (n - 1) as f32).round() as usize;
            let point = &gap.points[idx.min(n - 1)];
            painter.vline(x_of(idx), price.y_range(), Stroke::new(1.0, theme::border()));
            painter.vline(x_of(idx), rate.y_range(), Stroke::new(1.0, theme::border()));
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
        ui.label(RichText::new("typical same-direction path").font(ui_font(11.0)).color(muted()));
        ui.label(RichText::new("—").color(theme::up()));
        ui.label(RichText::new("share already filled").font(ui_font(11.0)).color(muted()));
    });
}

fn draw_optional(
    painter: &egui::Painter,
    gap: &GapFill,
    x_of: &impl Fn(usize) -> f32,
    y_of: &impl Fn(f64) -> f32,
    pick: impl Fn(&GapPoint) -> Option<f64>,
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
    for (i, point) in gap.points.iter().enumerate() {
        if let Some(value) = pick(point) {
            run.push(pos2(x_of(i), y_of(value)));
        } else {
            flush(painter, &mut run);
        }
    }
    flush(painter, &mut run);
}

fn hover_text(point: &GapPoint) -> String {
    let today = point.today.map(signed_pct).unwrap_or_else(|| "—".into());
    let typical = point.typical.map(signed_pct).unwrap_or_else(|| "—".into());
    let filled = match point.filled {
        Some(true) => "yes",
        Some(false) => "no",
        None => "—",
    };
    let rate = point.fill_rate.map(|value| format!("{:.0}%", value * 100.0)).unwrap_or_else(|| "—".into());
    format!(
        "{}   today {today}   typical {typical}   filled {filled}   rate {rate}   n={}",
        gap_fill::clock_label(point.ts),
        point.samples
    )
}

fn summary_line(gap: &GapFill) -> String {
    let levels = format!("{:.2} open vs {:.2} close on {}", gap.session_open, gap.prior_close, gap.prior_date);
    let head = if gap.direction == GapDirection::Flat {
        format!("No gap · {levels}")
    } else {
        format!("{} {:+.2}% · {levels}", gap.direction.label(), gap.gap_pct)
    };
    let state = if gap.direction == GapDirection::Flat {
        "the open equals the prior close".to_string()
    } else if gap.filled {
        let when = gap.fill_ts.map(gap_fill::clock_label).unwrap_or_else(|| "—".into());
        format!("filled at {when}")
    } else {
        let when = gap.today_last_ts.map(gap_fill::clock_label).unwrap_or_else(|| "—".into());
        let dist = gap.remaining_pct.map(signed_pct).unwrap_or_else(|| "—".into());
        format!("still {dist} from the close at {when}")
    };
    let rate = match gap.fill_rate_at_last {
        Some(value) => format!("{:.0}% of same-direction gaps had filled by the last print", value * 100.0),
        None => "fill rate blank at the last print".into(),
    };
    format!("{head} · {state} · {rate}")
}

fn signed_pct(value: f64) -> String {
    format!("{value:+.2}%")
}

fn y_bounds(gap: &GapFill) -> (f64, f64) {
    let mut lo = 0.0_f64;
    let mut hi = 0.0_f64;
    for point in &gap.points {
        for value in [point.today, point.typical].into_iter().flatten() {
            lo = lo.min(value);
            hi = hi.max(value);
        }
    }
    if hi - lo < 1.0 {
        let mid = (hi + lo) / 2.0;
        lo = mid - 0.5;
        hi = mid + 0.5;
    }
    let pad = (hi - lo) * 0.08;
    (lo - pad, hi + pad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::command::{Command, Function};
    use chrono::NaiveDate;
    use tr_core::{Bar, Provenance, Timeframe};
    use tr_data::gap_fill::{GapQuery, build_gap};
    use tr_data::intraday::regular_opens;

    #[test]
    fn gfill_commands_open_the_pane_and_request_a_gap() {
        let mut app = test_app();
        app.apply(Function::GapFill { spec: "40 15m".into() });
        let pane = app
            .panes
            .iter()
            .find_map(|pane| match &pane.kind {
                PaneKind::GapFill(pane) => Some(pane),
                _ => None,
            })
            .expect("gap fill pane");
        assert_eq!(pane.spec, "40 15m");
        assert_eq!(pane.tf, "5m");
        assert_eq!(crate::command::parse("GFILL"), Some(Command::Function(Function::GapFill { spec: String::new() })));
        assert_eq!(
            crate::command::parse("AAPL GFILL 40 15m"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::GapFill { spec: "40 15m".into() }))
        );
        assert!(matches!(crate::command::parse("GAPFILL"), Some(Command::Function(Function::GapFill { .. }))));

        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = GapFillPane::new("AAPL", "40 15m");
        pane.request(7, &worker);
        match requests.try_recv().unwrap() {
            Request::GapFill { pane: id, symbol, spec, timeframe, generation } => {
                assert_eq!(id, 7);
                assert_eq!(generation, 1);
                assert_eq!(symbol, "AAPL US Equity");
                assert_eq!(spec, "40 15m");
                assert_eq!(timeframe, "5m");
            }
            _ => panic!("unexpected request"),
        }
    }

    #[test]
    fn gap_fill_pane_rejects_sample_charts() {
        let mut date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let mut days = Vec::new();
        while days.len() < 2 {
            if tr_data::history::calendar::is_session(date) {
                days.push(date);
            }
            date += chrono::Duration::days(1);
        }
        let mut bars = Vec::new();
        for (i, day) in days.iter().enumerate() {
            for ts in regular_opens(*day, Timeframe::M30) {
                let (open, high, low, close) =
                    if i == 0 { (100.0, 100.0, 100.0, 100.0) } else { (110.0, 110.0, 105.0, 108.0) };
                bars.push(Bar::new(ts, open, high, low, close, 1.0).with_provenance(Provenance::Real));
            }
        }
        let mut gap = build_gap("AAPL", &bars, &GapQuery { window: 5, timeframe: Timeframe::M30 }, "unit").unwrap();
        gap.provenance = Provenance::Sample;
        let mut pane = GapFillPane::new("AAPL", "5");
        pane.generation = 1;
        pane.accept(1, Ok(gap));
        assert!(pane.gap.is_none());
        assert!(pane.error.as_deref().unwrap().contains("SAMPLE"));
    }
}
