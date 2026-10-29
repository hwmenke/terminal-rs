//! Strategy backtest pane (`BACKTEST`, `BT`) versus buy-and-hold.
use super::{PaneKind, TerminalApp};
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{RichText, ScrollArea, Ui};
use tr_data::backtest::{self, BacktestReport, StrategyResult};

pub struct BacktestPane {
    pub symbol: String,
    pub strategies: String,
    pub holdout: String,
    pub commission: String,
    pub slippage: String,
    pub allow_short: bool,
    /// Session VWAP and opening-range comparison. The daily archive path stays false.
    pub intraday: bool,
    /// Intraday bar size. Empty on a daily archive backtest.
    pub tf: String,
    pub generation: u64,
    pub report: Option<BacktestReport>,
    pub intraday_report: Option<tr_data::intraday_backtest::IntradayReport>,
    pub loading: bool,
    pub error: Option<String>,
}

impl BacktestPane {
    pub fn new(symbol: impl Into<String>, strategies: impl Into<String>) -> Self {
        Self::build(symbol, strategies, false)
    }

    pub fn intraday(symbol: impl Into<String>, strategies: impl Into<String>) -> Self {
        Self::build(symbol, strategies, true)
    }

    fn build(symbol: impl Into<String>, strategies: impl Into<String>, intraday: bool) -> Self {
        Self {
            symbol: symbol.into(),
            strategies: {
                let s = strategies.into();
                if s.trim().is_empty() { "ALL".into() } else { s }
            },
            holdout: "20%".into(),
            commission: format!("{:.0}", backtest::DEFAULT_COMMISSION_BPS),
            slippage: format!("{:.0}", backtest::DEFAULT_SLIPPAGE_BPS),
            allow_short: false,
            intraday,
            tf: if intraday { "5m".into() } else { String::new() },
            generation: 0,
            report: None,
            intraday_report: None,
            loading: false,
            error: None,
        }
    }

    pub fn accept(&mut self, generation: u64, result: Result<BacktestReport, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(report) => {
                if let Err(e) = report.provenance.require_verified("Backtest") {
                    self.error = Some(e);
                    self.report = None;
                    return;
                }
                self.error = None;
                self.report = Some(report);
            }
            Err(e) => {
                self.error = Some(e);
                self.report = None;
            }
        }
    }

    pub fn accept_intraday(
        &mut self,
        generation: u64,
        result: Result<tr_data::intraday_backtest::IntradayReport, String>,
    ) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(report) => {
                if let Err(e) = report.provenance.require_verified("Intraday backtest") {
                    self.error = Some(e);
                    self.intraday_report = None;
                    return;
                }
                self.error = None;
                self.intraday_report = Some(report);
            }
            Err(e) => {
                self.error = Some(e);
                self.intraday_report = None;
            }
        }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        let symbol = self.symbol.trim();
        if symbol.is_empty() {
            self.error = Some(if self.intraday {
                "IBT needs a ticker (AAPL IBT). Sample/display prices are never used.".into()
            } else {
                "BACKTEST needs a ticker (AAPL BACKTEST). Sample/display prices are never used.".into()
            });
            self.loading = false;
            return;
        }
        self.generation += 1;
        self.loading = true;
        self.error = None;
        self.report = None;
        self.intraday_report = None;
        let commission_bps = backtest::parse_bps(&self.commission, backtest::DEFAULT_COMMISSION_BPS);
        let slippage_bps = backtest::parse_bps(&self.slippage, backtest::DEFAULT_SLIPPAGE_BPS);
        if self.intraday {
            worker.send(Request::IntradayBacktest {
                pane,
                generation: self.generation,
                symbol: tr_data::security(symbol).key(),
                spec: self.strategies.clone(),
                timeframe: self.tf.clone(),
                holdout: self.holdout.clone(),
                commission_bps,
                slippage_bps,
                allow_short: self.allow_short,
            });
        } else {
            worker.send(Request::Backtest {
                pane,
                generation: self.generation,
                symbol: tr_data::security(symbol).key(),
                spec: self.strategies.clone(),
                holdout: self.holdout.clone(),
                commission_bps,
                slippage_bps,
                allow_short: self.allow_short,
            });
        }
    }
}

impl TerminalApp {
    pub(super) fn open_backtest(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        let mut pane = BacktestPane::new(symbol, spec);
        if pane.symbol.is_empty() {
            pane.error = Some("BACKTEST needs a ticker (AAPL BACKTEST). Sample/display prices are never used.".into());
        }
        let ticker = if pane.symbol.is_empty() { "Backtest".into() } else { tr_data::security(&pane.symbol).ticker };
        self.status = format!("{ticker}: paper backtest vs buy-and-hold (evaluate holdout, costs on)");
        self.open_panel(PaneKind::Backtest(Box::new(pane)));
    }

    pub(super) fn open_intraday_backtest(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        let mut pane = BacktestPane::intraday(symbol, spec);
        if pane.symbol.is_empty() {
            pane.error = Some("IBT needs a ticker (AAPL IBT). Sample/display prices are never used.".into());
        }
        let ticker = if pane.symbol.is_empty() { "Intraday".into() } else { tr_data::security(&pane.symbol).ticker };
        self.status = format!("{ticker}: paper intraday VWAP and opening range vs holding the cash session");
        self.open_panel(PaneKind::Backtest(Box::new(pane)));
    }

    pub(super) fn backtest_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let mut actions = Vec::new();
        let i = self.idx(id).expect("backtest pane");
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        let mut reload = false;
        let mut export = false;
        {
            let PaneKind::Backtest(pane) = &mut self.panes[i].kind else { return vec![] };
            let intraday = pane.intraday;
            if !pane.loading
                && pane.report.is_none()
                && pane.intraday_report.is_none()
                && pane.error.is_none()
                && !pane.symbol.trim().is_empty()
            {
                pane.request(id, &self.worker);
            }

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(if intraday { "Intraday" } else { "Backtest" }).strong().color(text()));
                ui.label(RichText::new("paper / research only").font(mono(11.0)).color(amber()));
                if ui
                    .small_button("Reload")
                    .on_hover_text(if intraday {
                        "Recompute from verified regular-session bars"
                    } else {
                        "Recompute from the research archive"
                    })
                    .clicked()
                {
                    reload = true;
                }
                if ui
                    .small_button("Export")
                    .on_hover_text("Write backtest JSON with source, as-of, costs and holdout (EXPORT BACKTEST)")
                    .clicked()
                {
                    export = true;
                }
            });
            ui.label(
                RichText::new(if intraday {
                    "Compare session VWAP and the opening range to holding each cash session. Signals fill at the next regular open and flatten at that session's close. The last fraction of sessions is unseen. Do not pick a winner from this table."
                } else {
                    "Compare declared strategies to buy-and-hold on the same unseen evaluate window. Costs and next-open fills are applied. Do not pick a winner from this table and treat it as a new out-of-sample test."
                })
                .font(ui_font(11.5))
                .color(muted()),
            );
            if sample_offline {
                ui.label(
                    RichText::new(
                        "Sample / offline display mode does not feed this pane. Backtests never fall back to sample bars.",
                    )
                    .font(ui_font(12.0))
                    .color(theme::down()),
                );
            }

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Ticker").color(muted()));
                let changed = ui
                    .add(egui::TextEdit::singleline(&mut pane.symbol).desired_width(110.0).hint_text("AAPL"))
                    .lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if changed {
                    reload = true;
                }
                ui.label(RichText::new("Strategies").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.strategies).desired_width(200.0).hint_text(if intraday {
                    "ALL, VWAP, OR"
                } else {
                    "ALL or ADTREND, SMA 20 50, RSI"
                }));
                if intraday {
                    ui.label(RichText::new("Bars").color(muted()));
                    ui.add(egui::TextEdit::singleline(&mut pane.tf).desired_width(48.0).hint_text("5m"));
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Holdout").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.holdout).desired_width(48.0).hint_text("20%"));
                ui.label(
                    RichText::new(if intraday { "last fraction of sessions" } else { "last fraction, chronological" })
                        .font(ui_font(11.0))
                        .color(muted()),
                );
                ui.label(RichText::new("Comm bps").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.commission).desired_width(36.0));
                ui.label(RichText::new("Slip bps").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.slippage).desired_width(36.0));
                if ui.selectable_label(pane.allow_short, "Shorts").clicked() {
                    pane.allow_short = !pane.allow_short;
                    reload = true;
                }
                if ui.button(RichText::new("RUN").font(mono(11.5))).clicked() {
                    reload = true;
                }
            });
            ui.horizontal_wrapped(|ui| {
                let chips: &[(&str, &str)] = if intraday {
                    &[("ALL", "ALL"), ("Hold", "HOLD"), ("VWAP", "VWAP"), ("OR", "OR")]
                } else {
                    &[
                        ("ALL", "ALL"),
                        ("Hold", "HOLD"),
                        ("ADTREND", "ADTREND"),
                        ("SMA 20/50", "SMA 20 50"),
                        ("RSI", "RSI"),
                    ]
                };
                for (label, line) in chips {
                    if ui.selectable_label(pane.strategies.trim().eq_ignore_ascii_case(line), *label).clicked() {
                        pane.strategies = (*line).into();
                        reload = true;
                    }
                }
            });

            if let Some(err) = &pane.error {
                ui.label(RichText::new(err).font(mono(12.0)).color(theme::down()));
            } else if pane.loading {
                ui.label(
                    RichText::new(if intraday {
                        "Reading verified intraday bars…"
                    } else {
                        "Reading research archive…"
                    })
                    .font(mono(12.0))
                    .color(muted()),
                );
            }

            if intraday {
                if let Some(report) = &pane.intraday_report {
                    render_intraday(ui, report);
                }
            } else if let Some(report) = &pane.report {
                render_report(ui, report);
            }
        }

        if reload {
            if let PaneKind::Backtest(pane) = &mut self.panes[i].kind {
                pane.request(id, &self.worker);
            }
        }
        if export {
            actions.push(Action::Command("EXPORT BACKTEST".into()));
        }
        actions
    }
}

fn render_report(ui: &mut Ui, report: &BacktestReport) {
    ui.label(
        RichText::new(format!(
            "{} · {} · as-of {} · {} … {} · {} daily bars",
            report.provenance.label(),
            report.source,
            report.as_of,
            report.series_start,
            report.series_end,
            report.bars
        ))
        .font(mono(11.5))
        .color(if report.provenance.is_verified() { muted() } else { theme::down() }),
    );
    ui.label(RichText::new(&report.settings_line).font(mono(11.0)).color(muted()));
    ui.label(
        RichText::new(format!(
            "Train {} … {} ({} bars, in-sample)   ·   Evaluate {} … {} ({} bars, unseen)",
            report.train_start,
            report.train_end,
            report.train_bars,
            report.evaluate_start,
            report.evaluate_end,
            report.evaluate_bars
        ))
        .font(mono(11.5))
        .color(text()),
    );
    ui.label(RichText::new(&report.execution_note).font(ui_font(11.0)).color(muted()));
    ui.label(RichText::new(&report.holdout_note).font(ui_font(11.0)).color(muted()));
    ui.label(RichText::new(&report.metrics_note).font(ui_font(11.0)).color(muted()));
    ui.label(RichText::new(&report.selection_note).font(ui_font(11.0)).color(amber()));

    let bh_cagr = report.results.iter().find(|r| r.benchmark).and_then(|r| r.evaluate.cagr);

    ui.add_space(6.0);
    ui.label(RichText::new("EVALUATE (unseen holdout)").font(mono(11.0)).color(muted()));
    header_row(ui);
    ScrollArea::vertical().id_salt("backtest-eval").auto_shrink([false, false]).show(ui, |ui| {
        for row in &report.results {
            strategy_row(ui, row, bh_cagr, true);
        }
        ui.add_space(10.0);
        ui.label(RichText::new("TRAIN (in-sample — not for selection)").font(mono(11.0)).color(muted()));
        header_row(ui);
        for row in &report.results {
            strategy_row(ui, row, None, false);
        }
        ui.add_space(8.0);
        ui.label(RichText::new("COMPLETED TRADES (evaluate)").font(mono(11.0)).color(muted()));
        for row in &report.results {
            let t = &row.evaluate.trades;
            ui.label(
                RichText::new(format!(
                    "{:<28}  closed {:>3}  wins {:>3}  win rate {}  avg win {}  avg loss {}  PF {}  hold {}d  open {} ({})",
                    trunc(&row.label, 28),
                    t.completed,
                    t.winners,
                    pct(t.completed_win_rate, 100.0),
                    pct(t.avg_win, 100.0),
                    pct(t.avg_loss, 100.0),
                    num(t.profit_factor),
                    num(t.avg_bars_held),
                    t.open_trades,
                    pct(t.open_unrealized, 100.0)
                ))
                .font(mono(11.5))
                .color(text()),
            );
            if let Some(hd) = t.holding_day_positive_rate {
                ui.label(
                    RichText::new(format!(
                        "{:<28}  holding-day positive {:.1}%  — not a win rate",
                        "",
                        hd * 100.0
                    ))
                    .font(mono(11.0))
                    .color(muted()),
                );
            }
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new("AAPL BACKTEST   ·   AAPL BT ADTREND   ·   EXPORT BACKTEST   ·   see docs/BACKTEST.md")
                .font(ui_font(11.0))
                .color(muted()),
        );
    });
}

fn header_row(ui: &mut Ui) {
    ui.label(
        RichText::new(format!(
            "{:<28} {:>8} {:>8} {:>8} {:>6} {:>8} {:>8} {:>8}",
            "Strategy", "CAGR", "Total", "Max DD", "Exp%", "Closed", "Win%", "vs B&H"
        ))
        .font(mono(11.0))
        .color(muted()),
    );
}

fn strategy_row(ui: &mut Ui, row: &StrategyResult, bh_cagr: Option<f64>, evaluate: bool) {
    let m = if evaluate { &row.evaluate } else { &row.train };
    let vs = if evaluate {
        match (m.cagr, bh_cagr) {
            (Some(a), Some(b)) if !row.benchmark => format!("{:+.1}%", (a - b) * 100.0),
            _ if row.benchmark => "bench".into(),
            _ => "—".into(),
        }
    } else {
        "n/a".into()
    };
    let color = if row.benchmark { amber() } else { text() };
    ui.label(
        RichText::new(format!(
            "{:<28} {:>8} {:>8} {:>8} {:>6} {:>8} {:>8} {:>8}",
            trunc(&row.label, 28),
            pct(m.cagr, 100.0),
            pct(m.total_return, 100.0),
            pct(m.max_drawdown, 100.0),
            pct(m.exposure, 100.0),
            m.trades.completed,
            pct(m.trades.completed_win_rate, 100.0),
            vs
        ))
        .font(mono(12.0))
        .color(color),
    );
    ui.label(RichText::new(format!("  {}", row.params)).font(mono(10.5)).color(muted()));
}

fn pct(v: Option<f64>, scale: f64) -> String {
    v.map(|x| format!("{:.1}%", x * scale)).unwrap_or_else(|| "—".into())
}

fn num(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "—".into())
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n.saturating_sub(1)).collect::<String>() + "…" }
}

fn render_intraday(ui: &mut Ui, report: &tr_data::intraday_backtest::IntradayReport) {
    ui.label(
        RichText::new(format!(
            "{} · {} · as-of {} · {} … {} · {} {} sessions · {} bars",
            report.provenance.label(),
            report.source,
            report.as_of,
            report.series_start,
            report.series_end,
            report.sessions,
            report.timeframe.label(),
            report.bars
        ))
        .font(mono(11.5))
        .color(if report.provenance.is_verified() { muted() } else { theme::down() }),
    );
    ui.label(RichText::new(&report.settings_line).font(mono(11.0)).color(muted()));
    ui.label(
        RichText::new(format!(
            "Train {} … {} ({} sessions, in-sample)   ·   Evaluate {} … {} ({} sessions, unseen)",
            report.train_start,
            report.train_end,
            report.train_sessions,
            report.evaluate_start,
            report.evaluate_end,
            report.evaluate_sessions
        ))
        .font(mono(11.5))
        .color(text()),
    );
    ui.label(RichText::new(&report.execution_note).font(ui_font(11.0)).color(muted()));
    ui.label(RichText::new(&report.holdout_note).font(ui_font(11.0)).color(muted()));
    ui.label(RichText::new(&report.metrics_note).font(ui_font(11.0)).color(muted()));
    ui.label(RichText::new(&report.skip_note).font(ui_font(11.0)).color(muted()));
    ui.label(RichText::new(&report.opening_note).font(ui_font(11.0)).color(muted()));
    ui.label(RichText::new(&report.selection_note).font(ui_font(11.0)).color(amber()));

    ui.add_space(6.0);
    ui.label(RichText::new("EVALUATE (unseen sessions)").font(mono(11.0)).color(muted()));
    intraday_header(ui);
    ScrollArea::vertical().id_salt("ibt-eval").auto_shrink([false, false]).show(ui, |ui| {
        for row in &report.results {
            intraday_row(ui, row, true);
        }
        ui.add_space(10.0);
        ui.label(RichText::new("TRAIN (in-sample — not for selection)").font(mono(11.0)).color(muted()));
        intraday_header(ui);
        for row in &report.results {
            intraday_row(ui, row, false);
        }
        ui.add_space(8.0);
        ui.label(RichText::new("COMPLETED ROUND-TRIPS (evaluate)").font(mono(11.0)).color(muted()));
        for row in &report.results {
            let t = &row.evaluate.trades;
            ui.label(
                RichText::new(format!(
                    "{:<16}  closed {:>3}  wins {:>3}  win rate {}  open {}  skipped OR {}",
                    trunc(&row.label, 16),
                    t.completed,
                    t.winners,
                    pct(t.completed_win_rate, 100.0),
                    t.open_trades,
                    row.evaluate.opening_skipped
                ))
                .font(mono(11.5))
                .color(text()),
            );
            if let Some(hd) = t.holding_bar_positive_rate {
                ui.label(
                    RichText::new(format!("    holding-bar positive {:.1}%  — not a win rate", hd * 100.0))
                        .font(mono(11.0))
                        .color(muted()),
                );
            }
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new("AAPL IBT   ·   AAPL IBT VWAP   ·   AAPL IBT OR 15m   ·   EXPORT BACKTEST   ·   see docs/INTRADAY_BACKTEST.md")
                .font(ui_font(11.0))
                .color(muted()),
        );
    });
}

fn intraday_header(ui: &mut Ui) {
    ui.label(
        RichText::new(format!(
            "{:<16} {:>8} {:>8} {:>8} {:>6} {:>8} {:>8} {:>8}",
            "Setup", "CAGR", "Total", "Max DD", "Exp%", "Closed", "Win%", "vs Hold"
        ))
        .font(mono(11.0))
        .color(muted()),
    );
}

fn intraday_row(ui: &mut Ui, row: &tr_data::intraday_backtest::SetupResult, evaluate: bool) {
    let m = if evaluate { &row.evaluate } else { &row.train };
    let vs = if evaluate {
        match row.vs_hold {
            Some(gap) if !row.benchmark => format!("{:+.1}%", gap * 100.0),
            _ if row.benchmark => "bench".into(),
            _ => "—".into(),
        }
    } else {
        "n/a".into()
    };
    let color = if row.benchmark { amber() } else { text() };
    ui.label(
        RichText::new(format!(
            "{:<16} {:>8} {:>8} {:>8} {:>6} {:>8} {:>8} {:>8}",
            trunc(&row.label, 16),
            pct(m.session_cagr, 100.0),
            pct(m.total_return, 100.0),
            pct(m.max_drawdown, 100.0),
            pct(m.exposure, 100.0),
            m.trades.completed,
            pct(m.trades.completed_win_rate, 100.0),
            vs
        ))
        .font(mono(12.0))
        .color(color),
    );
    ui.label(RichText::new(format!("  {}", row.params)).font(mono(10.5)).color(muted()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::command::{Command, Function};

    #[test]
    fn backtest_commands_open_the_pane() {
        let mut app = test_app();
        app.apply(Function::Backtest { spec: String::new() });
        assert!(app.panes.iter().any(|p| matches!(p.kind, PaneKind::Backtest(_))));
        assert_eq!(
            crate::command::parse("BACKTEST"),
            Some(Command::Function(Function::Backtest { spec: String::new() }))
        );
        assert_eq!(
            crate::command::parse("AAPL BACKTEST"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Backtest { spec: String::new() }))
        );
        assert_eq!(
            crate::command::parse("AAPL BT ADTREND"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Backtest { spec: "ADTREND".into() }))
        );
        assert_eq!(parse_alias("BT"), true);
    }

    #[test]
    fn ibt_commands_open_an_intraday_pane() {
        let mut app = test_app();
        app.apply(Function::IntradayBacktest { spec: "VWAP".into() });
        let pane = app
            .panes
            .iter()
            .find_map(|p| match &p.kind {
                PaneKind::Backtest(b) if b.intraday => Some(b),
                _ => None,
            })
            .expect("intraday pane");
        assert_eq!(pane.strategies, "VWAP");
        assert_eq!(pane.tf, "5m");
        assert_eq!(
            crate::command::parse("IBT"),
            Some(Command::Function(Function::IntradayBacktest { spec: String::new() }))
        );
        assert_eq!(
            crate::command::parse("AAPL IBT OR 15m"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::IntradayBacktest { spec: "OR 15m".into() }
            ))
        );
        assert!(matches!(
            crate::command::parse("IBACKTEST"),
            Some(Command::Function(Function::IntradayBacktest { .. }))
        ));
    }

    #[test]
    fn intraday_pane_rejects_sample_reports() {
        let mut pane = BacktestPane::intraday("AAPL US Equity", "HOLD");
        pane.generation = 1;
        let mut report = intraday_dummy();
        report.provenance = tr_core::Provenance::Sample;
        pane.accept_intraday(1, Ok(report));
        assert!(pane.intraday_report.is_none());
        assert!(pane.error.as_deref().unwrap().contains("SAMPLE"));
    }

    fn intraday_dummy() -> tr_data::intraday_backtest::IntradayReport {
        use chrono::{Duration, NaiveDate};
        use tr_core::{Bar, Provenance, Timeframe};
        use tr_data::history::calendar::is_session;
        use tr_data::intraday::regular_opens;
        let mut date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let mut days = Vec::new();
        while days.len() < 12 {
            if is_session(date) {
                days.push(date);
            }
            date += Duration::days(1);
        }
        let bars: Vec<Bar> = days
            .into_iter()
            .flat_map(|date| {
                regular_opens(date, Timeframe::M30)
                    .into_iter()
                    .map(|ts| Bar::new(ts, 10.0, 10.1, 9.9, 10.0, 100.0).with_provenance(Provenance::Real))
            })
            .collect();
        let settings = tr_data::intraday_backtest::RunSettings {
            timeframe: Timeframe::M30,
            setups: vec![tr_data::intraday_backtest::Setup::SessionHold],
            ..tr_data::intraday_backtest::RunSettings::default()
        };
        tr_data::intraday_backtest::compute_report("AAPL US Equity", &bars, &settings, "unit").unwrap()
    }

    fn parse_alias(code: &str) -> bool {
        matches!(crate::command::parse(code), Some(Command::Function(Function::Backtest { .. })))
    }

    #[test]
    fn backtest_pane_rejects_sample_reports() {
        let mut pane = BacktestPane::new("AAPL US Equity", "HOLD");
        pane.generation = 1;
        let mut report = dummy_report();
        report.provenance = tr_core::Provenance::Sample;
        pane.accept(1, Ok(report));
        assert!(pane.report.is_none());
        assert!(pane.error.as_deref().unwrap().contains("SAMPLE"));
    }

    fn dummy_report() -> BacktestReport {
        let ts = 1_700_000_000;
        let bars: Vec<tr_core::Bar> = (0..80)
            .map(|i| {
                let c = 10.0 + i as f64 * 0.05;
                tr_core::Bar::new(ts + i * 86_400, c, c, c, c, 1.0).with_provenance(tr_core::Provenance::Real)
            })
            .collect();
        let settings = backtest::RunSettings::default().with_strategies(vec![backtest::StrategySpec::BuyAndHold]);
        backtest::compute_report("AAPL US Equity", "AAPL", &bars, &settings, "unit").unwrap()
    }
}
