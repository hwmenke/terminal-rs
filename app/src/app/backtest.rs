//! Strategy backtest pane (`BACKTEST`, `BT`) versus buy-and-hold.
mod honesty;
use super::{PaneKind, TerminalApp};
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{RichText, ScrollArea, Ui};
use std::{collections::BTreeMap, sync::Arc};
use tr_data::backtest::{self, BacktestReport, StrategyResult};
use tr_data::backtest_honesty::{HonestReport, HonestySettings};

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
    pub report: Option<Arc<BacktestReport>>,
    pub intraday_report: Option<Arc<tr_data::intraday_backtest::IntradayReport>>,
    pub honesty: HonestySettings,
    pub holdout_tables: bool,
    pub selected_variant: usize,
    pub selected_fold: usize,
    pub receipt: String,
    snapshot: Option<Controls>,
    revisions: BTreeMap<String, u64>,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Clone, PartialEq)]
struct Controls {
    symbol: String,
    strategies: String,
    holdout: String,
    commission: String,
    slippage: String,
    shorts: bool,
    tf: String,
    honesty: HonestySettings,
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
            honesty: HonestySettings::default(),
            holdout_tables: false,
            selected_variant: 0,
            selected_fold: 0,
            receipt: String::new(),
            snapshot: None,
            revisions: BTreeMap::new(),
            generation: 0,
            report: None,
            intraday_report: None,
            loading: false,
            error: None,
        }
    }

    pub fn accept(&mut self, generation: u64, result: Result<(Arc<BacktestReport>, String), String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok((report, receipt)) => {
                if self.stale_family(report.honesty.as_ref()) {
                    return;
                }
                self.receipt = receipt;
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
        result: Result<(Arc<tr_data::intraday_backtest::IntradayReport>, String), String>,
    ) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok((report, receipt)) => {
                if self.stale_family(report.honesty.as_ref()) {
                    return;
                }
                self.receipt = receipt;
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

    fn controls(&self) -> Controls {
        Controls {
            symbol: self.symbol.clone(),
            strategies: self.strategies.clone(),
            holdout: self.holdout.clone(),
            commission: self.commission.clone(),
            slippage: self.slippage.clone(),
            shorts: self.allow_short,
            tf: self.tf.clone(),
            honesty: self.honesty,
        }
    }
    fn stale_family(&self, honest: Option<&HonestReport>) -> bool {
        honest
            .and_then(|h| h.trial_family.value())
            .is_some_and(|f| self.revisions.get(&f.scope).is_some_and(|r| *r > f.revision))
    }
    pub(super) fn note_revision(&mut self, scope: &str, revision: u64) -> bool {
        let floor = self.revisions.entry(scope.into()).or_default();
        *floor = (*floor).max(revision);
        let honest = self
            .report
            .as_ref()
            .and_then(|r| r.honesty.as_ref())
            .or_else(|| self.intraday_report.as_ref().and_then(|r| r.honesty.as_ref()));
        self.stale_family(honest) && !self.loading
    }
    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        self.snapshot = Some(self.controls());
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
        self.generation = crate::worker::studio_token();
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
                honesty: self.honesty,
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
                honesty: self.honesty,
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
        let i = self.idx(id).expect("research pane");
        let PaneKind::Backtest(pane) = &mut self.panes[i].kind else { return vec![] };
        show(ui, id, pane, &self.worker, self.provider.eq_ignore_ascii_case("sample"), false)
    }
}

/// Shared renderer/action adapter for the standalone pane and Studio shell.
pub(super) fn show(
    ui: &mut Ui,
    id: PaneId,
    pane: &mut BacktestPane,
    worker: &crate::worker::Worker,
    sample: bool,
    embedded: bool,
) -> Vec<Action> {
    let mut actions = Vec::new();
    let sample_offline = sample;
    let mut reload = false;
    let mut export = false;
    if sample {
        // Refuse locally, with no worker request: nothing from a real-mode run (including an
        // in-flight one) may stay visible while the display mode is sample / offline.
        if pane.report.is_some() || pane.intraday_report.is_some() || pane.loading {
            pane.generation = crate::worker::studio_token();
        }
        pane.report = None;
        pane.intraday_report = None;
        pane.receipt.clear();
        pane.snapshot = None;
        pane.loading = false;
        // A worker refusal that already names sample mode stays; any other (real-mode) error is replaced.
        if !pane.error.as_deref().is_some_and(|e| e.to_ascii_lowercase().contains("sample")) {
            pane.error =
                Some("Backtest refused: sample / offline display mode. Backtests never use sample bars.".into());
        }
    }
    {
        let intraday = pane.intraday;
        if !sample
            && !pane.loading
            && pane.report.is_none()
            && pane.intraday_report.is_none()
            && pane.error.is_none()
            && !pane.symbol.trim().is_empty()
        {
            pane.request(id, worker);
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
            if !embedded
                && ui
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
            if !embedded {
                ui.label(RichText::new("Ticker").color(muted()));
                let changed = ui
                    .add(egui::TextEdit::singleline(&mut pane.symbol).desired_width(110.0).hint_text("AAPL"))
                    .lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if changed {
                    reload = true;
                }
            }
            ui.label(RichText::new("Strategies").color(muted()));
            ui.add(egui::TextEdit::singleline(&mut pane.strategies).desired_width(200.0).hint_text(if intraday {
                "ALL, VWAP, OR"
            } else {
                "ALL or ADTREND, SMA 20 50, RSI"
            }));
            if intraday && !embedded {
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
                &[("ALL", "ALL"), ("Hold", "HOLD"), ("ADTREND", "ADTREND"), ("SMA 20/50", "SMA 20 50"), ("RSI", "RSI")]
            };
            for (label, line) in chips {
                if ui.selectable_label(pane.strategies.trim().eq_ignore_ascii_case(line), *label).clicked() {
                    pane.strategies = (*line).into();
                    reload = true;
                }
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Folds").color(muted()));
            ui.add(egui::DragValue::new(&mut pane.honesty.folds).range(2..=6));
            ui.label(RichText::new("Embargo sessions").color(muted()));
            ui.add(egui::DragValue::new(&mut pane.honesty.embargo_sessions).range(0..=252));
            if ui.selectable_label(!pane.holdout_tables, "Honest").clicked() {
                pane.holdout_tables = false;
            }
            if ui.selectable_label(pane.holdout_tables, "Holdout tables").clicked() {
                pane.holdout_tables = true;
            }
        });
        if pane.snapshot.as_ref().is_some_and(|s| *s != pane.controls()) {
            ui.label(
                RichText::new("CONTROLS CHANGED · RUN to apply. Evidence below uses the accepted run settings.")
                    .color(amber())
                    .font(mono(11.0)),
            );
        }
        if !pane.receipt.is_empty() {
            ui.label(RichText::new(&pane.receipt).color(muted()).font(mono(11.0)));
        }
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

        if sample_offline {
            // A provider-mode switch must not leave prior confidence visible
            // while its refusal response is still in flight.
        } else if pane.holdout_tables {
            if intraday {
                if let Some(report) = &pane.intraday_report {
                    render_intraday(ui, report);
                }
            } else if let Some(report) = &pane.report {
                render_report(ui, report);
            }
        } else {
            let honest = pane
                .report
                .as_ref()
                .and_then(|r| r.honesty.as_ref())
                .or_else(|| pane.intraday_report.as_ref().and_then(|r| r.honesty.as_ref()));
            if let Some(h) = honest {
                let accepted_symbol = pane
                    .report
                    .as_ref()
                    .map(|r| r.symbol.as_str())
                    .or_else(|| pane.intraday_report.as_ref().map(|r| r.symbol.as_str()))
                    .unwrap_or("");
                ui.label(RichText::new(format!("Evidence: {accepted_symbol}")).strong().color(text()));
                let settings_line = pane
                    .report
                    .as_ref()
                    .map(|r| r.settings_line.as_str())
                    .or_else(|| pane.intraday_report.as_ref().map(|r| r.settings_line.as_str()))
                    .unwrap_or("");
                ui.label(RichText::new(settings_line).color(muted()).font(mono(11.0)));
                honesty::render(ui, h, &mut pane.selected_variant, &mut pane.selected_fold);
            }
        }
    }

    if reload && !sample {
        pane.request(id, worker);
    }
    if export {
        actions.push(Action::Command("EXPORT BACKTEST".into()));
    }
    actions
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
        .color(theme::provenance(report.provenance)),
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
        .color(theme::provenance(report.provenance)),
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
        assert!(parse_alias("BT"));
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
        pane.accept_intraday(1, Ok((Arc::new(report), String::new())));
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
        pane.accept(1, Ok((Arc::new(report), String::new())));
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
    pub(super) fn honest_report() -> Arc<BacktestReport> {
        use tr_data::{
            backtest_honesty::Estimate,
            backtest_trials::{self, FamilySnapshot, Trial},
        };
        let mut date = "2025-01-02".parse().unwrap();
        let mut bars = Vec::new();
        for i in 0..180 {
            let p = 100. + i as f64 * 0.1 + (i as f64 * 0.4).sin() * 3.;
            bars.push(
                tr_core::Bar::new(
                    chrono::NaiveDate::and_hms_opt(&date, 0, 0, 0).unwrap().and_utc().timestamp(),
                    p,
                    p + 1.,
                    p - 1.,
                    p + 0.3,
                    100.,
                )
                .with_provenance(tr_core::Provenance::Real),
            );
            date = tr_data::history::calendar::next_session(date);
        }
        let settings = backtest::RunSettings::default().with_strategies(vec![
            backtest::StrategySpec::BuyAndHold,
            backtest::StrategySpec::SmaCross { fast: 3, slow: 8 },
        ]);
        let wf = HonestySettings::default();
        let experiments = backtest_trials::daily_experiments(&settings, wf);
        let family = FamilySnapshot {
            scope: "AAPL / BT / 1D / verified fixture".into(),
            revision: 4,
            tracked_since: bars[0].ts,
            digest: "fixture".into(),
            trials: experiments
                .iter()
                .map(|e| Trial {
                    id: e.canonical(),
                    experiment: e.clone(),
                    first_seen: bars[0].ts,
                    original_digest: "fixture".into(),
                    original_start: bars[0].ts,
                    original_end: bars.last().unwrap().ts,
                    status: "computed".into(),
                })
                .collect(),
        };
        let mut report =
            backtest::compute_report("AAPL US Equity", "AAPL", &bars, &settings, "verified fixture").unwrap();
        report.honesty = Some(backtest_trials::compute(
            &bars,
            tr_core::Timeframe::D1,
            wf,
            &experiments,
            Estimate::Available(family),
            "VERIFIED fixture".into(),
        ));
        Arc::new(report)
    }
    #[test]
    fn honest_backtest_paint_has_all_evidence_and_never_requests_again() {
        let mut app = test_app();
        app.open_backtest(Some("AAPL"), "ALL");
        app.provider = "verified fixture".into();
        let id = app.focus;
        let i = app.idx(id).unwrap();
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        app.worker = worker;
        let PaneKind::Backtest(p) = &mut app.panes[i].kind else { panic!("backtest") };
        p.report = Some(honest_report());
        p.selected_variant = 1;
        let ctx = egui::Context::default();
        for width in [900., 360.] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 1800.))),
                    ..Default::default()
                },
                |ui| {
                    app.backtest_pane(ui, id);
                },
            );
            let texts = output
                .shapes
                .iter()
                .filter_map(
                    |s| if let egui::Shape::Text(t) = &s.shape { Some(t.galley.job.text.as_str()) } else { None },
                )
                .collect::<Vec<_>>()
                .join("\n");
            for label in ["OOS EQUITY", "FOLD RIBBON", "DSR", "IS vs OOS", "COMMON COMPARISON", "FULL STITCHED OOS"] {
                assert!(texts.contains(label), "missing {label} at {width}");
            }
            output.textures_delta.clear();
            assert!(requests.try_recv().is_err());
        }
        app.provider = "sample".into();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            app.backtest_pane(ui, id);
        });
        let text = output
            .shapes
            .iter()
            .filter_map(|s| if let egui::Shape::Text(t) = &s.shape { Some(t.galley.job.text.as_str()) } else { None })
            .collect::<Vec<_>>()
            .join("\n");
        output.textures_delta.clear();
        assert!(!text.contains("OOS EQUITY"));
        assert!(!text.contains("COMMON COMPARISON"));
        let PaneKind::Backtest(p) = &mut app.panes[i].kind else { panic!() };
        p.accept(p.generation, Err("SAMPLE refused".into()));
        let mut output = ctx.run_ui(Default::default(), |ui| {
            app.backtest_pane(ui, id);
        });
        let texts = output
            .shapes
            .iter()
            .filter_map(|s| if let egui::Shape::Text(t) = &s.shape { Some(t.galley.job.text.as_str()) } else { None })
            .collect::<Vec<_>>()
            .join("\n");
        output.textures_delta.clear();
        assert!(texts.contains("SAMPLE refused"));
        assert!(!texts.contains("OOS EQUITY"));
        assert!(!texts.contains("COMMON COMPARISON"));
    }
    #[test]
    fn backtest_sample_mode_refuses_locally_without_requests() {
        let mut app = test_app();
        app.open_backtest(Some("AAPL"), "ALL");
        app.provider = "verified fixture".into();
        let id = app.focus;
        let i = app.idx(id).unwrap();
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        app.worker = worker;
        let PaneKind::Backtest(p) = &mut app.panes[i].kind else { panic!("backtest") };
        p.report = Some(honest_report());
        p.receipt = "OLD RECEIPT real-mode run".into();
        p.error = Some("earlier real-mode error".into());
        p.loading = true;
        let ctx = egui::Context::default();
        let paint = |app: &mut crate::app::TerminalApp| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900., 1800.))),
                    ..Default::default()
                },
                |ui| {
                    app.backtest_pane(ui, id);
                },
            );
            output.textures_delta.clear();
            output
                .shapes
                .iter()
                .filter_map(|s| if let egui::Shape::Text(t) = &s.shape { Some(t.galley.job.text.clone()) } else { None })
                .collect::<Vec<_>>()
                .join("\n")
        };
        app.provider = "sample".into();
        let text = paint(&mut app);
        assert!(text.contains("Backtest refused"), "{text}");
        assert!(!text.contains("OOS EQUITY") && !text.contains("OLD RECEIPT") && !text.contains("earlier real-mode"));
        // Reload / RUN and repeated frames stay local.
        let text = paint(&mut app);
        assert!(text.contains("Backtest refused"));
        assert!(requests.try_recv().is_err(), "sample mode must not send backtest requests");
        let PaneKind::Backtest(p) = &app.panes[i].kind else { panic!() };
        assert!(p.report.is_none() && p.receipt.is_empty() && !p.loading);
    }
    #[test]
    fn honest_backtest_revisions_and_generations_cannot_restore_stale_confidence() {
        let report = honest_report();
        let family = report.honesty.as_ref().unwrap().trial_family.value().unwrap();
        let mut p = BacktestPane::new("AAPL", "ALL");
        p.generation = 2;
        p.loading = true;
        p.accept(1, Ok((report.clone(), "old".into())));
        assert!(p.report.is_none());
        assert!(p.loading);
        assert!(!p.note_revision(&family.scope, family.revision + 1));
        p.accept(2, Ok((report.clone(), "stale".into())));
        assert!(p.report.is_none());
        assert!(!p.loading);
        let mut newest = (*report).clone();
        if let tr_data::backtest_honesty::Estimate::Available(f) = &mut newest.honesty.as_mut().unwrap().trial_family {
            f.revision += 1;
        }
        p.accept(2, Ok((Arc::new(newest), "current".into())));
        assert!(p.report.is_some());
        assert!(!p.note_revision(&family.scope, family.revision + 1));
        assert!(p.note_revision(&family.scope, family.revision + 2));
    }
    #[test]
    fn honest_backtest_keys_respect_focus_text_and_custom_bindings() {
        use egui::{Key, Modifiers};
        let mut app = test_app();
        app.open_backtest(Some("AAPL"), "ALL");
        app.focus_cmd = false;
        app.cmd.clear();
        let i = app.idx(app.focus).unwrap();
        let PaneKind::Backtest(p) = &mut app.panes[i].kind else { panic!() };
        p.report = Some(honest_report());
        let ctx = egui::Context::default();
        let key = |k| egui::RawInput {
            events: vec![egui::Event::Key {
                key: k,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        };
        for (k, want) in [(Key::PageDown, 1), (Key::End, 2), (Key::PageUp, 1), (Key::Home, 0)] {
            let mut output = ctx.run_ui(key(k), |ui| {
                assert!(app.backtest_keys(ui.ctx()));
            });
            output.textures_delta.clear();
            let PaneKind::Backtest(p) = &app.panes[i].kind else { panic!() };
            assert_eq!(p.selected_fold, want);
        }
        app.cmd = "AAPL".into();
        let mut output = ctx.run_ui(key(Key::PageDown), |ui| {
            assert!(!app.backtest_keys(ui.ctx()));
        });
        output.textures_delta.clear();
        app.cmd.clear();
        app.prefs.hotkeys.bind(crate::prefs::Action::Refresh, crate::prefs::Chord::new(Key::PageDown));
        let mut output = ctx.run_ui(key(Key::PageDown), |ui| {
            assert!(!app.backtest_keys(ui.ctx()));
        });
        output.textures_delta.clear();
        app.palette.open = true;
        let mut output = ctx.run_ui(key(Key::End), |ui| {
            assert!(!app.backtest_keys(ui.ctx()));
        });
        output.textures_delta.clear();
    }
    #[test]
    fn honest_backtest_layout_defaults_and_round_trip() {
        let old = serde_json::json!({"Backtest":{"symbol":"AAPL US Equity","spec":"ALL","holdout":"20%","commission":"5","slippage":"5","shorts":false,"intraday":false,"tf":""}});
        let settings: crate::layouts::PaneSettings = serde_json::from_value(old).unwrap();
        let PaneKind::Backtest(mut p) = PaneKind::from_settings(settings) else { panic!() };
        assert_eq!(p.honesty, HonestySettings::default());
        assert!(!p.holdout_tables);
        p.honesty = HonestySettings { folds: 5, embargo_sessions: 3 };
        p.holdout_tables = true;
        p.selected_fold = 3;
        p.selected_variant = 1;
        let saved = PaneKind::Backtest(p).settings();
        let decoded = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert_eq!(PaneKind::from_settings(decoded).settings(), saved);
    }
}
