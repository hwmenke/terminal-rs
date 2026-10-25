//! Statistics and conditional-return pane (`STATS`, `COND`).
use super::{PaneKind, TerminalApp};
use crate::theme::{self, amber, dp, mono, muted, text, ui_font};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{RichText, ScrollArea, Ui};
use tr_data::stats::{self, ConditionalReport, HorizonCompare, ResearchSnapshot, ReturnStats, StatsReport};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsTab {
    Overview,
    Conditional,
}

pub struct StatsPane {
    pub symbol: String,
    pub condition: String,
    pub horizons: String,
    pub tab: StatsTab,
    pub generation: u64,
    pub snapshot: Option<ResearchSnapshot>,
    pub loading: bool,
    pub error: Option<String>,
}

impl StatsPane {
    pub fn new(symbol: impl Into<String>, condition: impl Into<String>, conditional: bool) -> Self {
        Self {
            symbol: symbol.into(),
            condition: condition.into(),
            horizons: "5, 10".into(),
            tab: if conditional { StatsTab::Conditional } else { StatsTab::Overview },
            generation: 0,
            snapshot: None,
            loading: false,
            error: None,
        }
    }

    pub fn accept(&mut self, generation: u64, result: Result<ResearchSnapshot, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(snap) => {
                if let Err(e) = snap.stats.provenance.require_verified("Statistics") {
                    self.error = Some(e);
                    self.snapshot = None;
                    return;
                }
                if let Err(e) = snap.conditional.provenance.require_verified("Conditional returns") {
                    self.error = Some(e);
                    self.snapshot = None;
                    return;
                }
                self.error = None;
                self.snapshot = Some(snap);
            }
            Err(e) => {
                self.error = Some(e);
                self.snapshot = None;
            }
        }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        let symbol = self.symbol.trim();
        if symbol.is_empty() {
            self.error = Some("STATS needs a ticker (AAPL STATS, or type a name here).".into());
            self.loading = false;
            return;
        }
        self.generation += 1;
        self.loading = true;
        self.error = None;
        worker.send(Request::Stats {
            pane,
            generation: self.generation,
            symbol: tr_data::security(symbol).key(),
            condition: self.condition.clone(),
            horizons: parse_horizons(&self.horizons),
        });
    }
}

fn parse_horizons(s: &str) -> Vec<usize> {
    let hs: Vec<usize> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter_map(|t| t.parse().ok())
        .filter(|h| *h > 0 && *h <= 252)
        .collect();
    if hs.is_empty() { stats::DEFAULT_HORIZONS.to_vec() } else { hs }
}

impl TerminalApp {
    pub(super) fn open_stats(&mut self, symbol: Option<&str>, condition: &str, conditional: bool) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        let mut pane = StatsPane::new(symbol, condition, conditional);
        if pane.symbol.is_empty() {
            pane.error = Some("STATS needs a ticker (AAPL STATS). Sample/display prices are never used.".into());
        }
        let ticker = if pane.symbol.is_empty() { "Stats".into() } else { tr_data::security(&pane.symbol).ticker };
        self.status = if conditional {
            if condition.trim().is_empty() {
                format!("{ticker}: conditional returns (baseline = every completed bar)")
            } else {
                format!("{ticker}: what followed {condition}")
            }
        } else {
            format!("{ticker}: historical risk and returns")
        };
        self.open_panel(PaneKind::Stats(Box::new(pane)));
    }

    pub(super) fn stats_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let mut actions = Vec::new();
        let i = self.idx(id).expect("stats pane");
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        let mut reload = false;
        let mut export = false;
        {
            let PaneKind::Stats(pane) = &mut self.panes[i].kind else { return vec![] };
            if !pane.loading && pane.snapshot.is_none() && pane.error.is_none() && !pane.symbol.trim().is_empty() {
                pane.request(id, &self.worker);
            }

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Statistics").strong().color(text()));
                for (tab, label) in [(StatsTab::Overview, "Overview"), (StatsTab::Conditional, "Conditional")] {
                    if ui.selectable_label(pane.tab == tab, label).clicked() {
                        pane.tab = tab;
                    }
                }
                if ui.small_button("Reload").on_hover_text("Recompute from the research archive").clicked() {
                    reload = true;
                }
                if ui
                    .small_button("Export")
                    .on_hover_text("Write stats JSON with source, as-of and provenance (EXPORT STATS)")
                    .clicked()
                {
                    export = true;
                }
            });
            ui.label(
                RichText::new(
                    "Compare a name's historical risk and returns, then see what followed conditions such as low RSI or price above a moving average. Numbers use verified daily archive bars only.",
                )
                .font(ui_font(11.5))
                .color(muted()),
            );
            if sample_offline {
                ui.label(
                    RichText::new(
                        "Sample / offline display mode does not feed this pane. Statistics never fall back to sample bars.",
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
            });

            if let Some(err) = &pane.error {
                ui.label(RichText::new(err).font(mono(12.0)).color(theme::down()));
            } else if pane.loading {
                ui.label(RichText::new("Reading research archive…").font(mono(12.0)).color(muted()));
            }

            if let Some(snap) = &pane.snapshot {
                provenance_line(ui, &snap.stats);
            }

            match pane.tab {
                StatsTab::Overview => {
                    if let Some(snap) = &pane.snapshot {
                        overview(ui, &snap.stats);
                    }
                }
                StatsTab::Conditional => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Condition").color(muted()));
                        ui.add(
                            egui::TextEdit::singleline(&mut pane.condition)
                                .desired_width(280.0)
                                .hint_text("RSI(14) < 30 AND close > SMA(50)"),
                        );
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Horizons").color(muted()));
                        ui.add(egui::TextEdit::singleline(&mut pane.horizons).desired_width(72.0).hint_text("5, 10"));
                        ui.label(RichText::new("completed trading days").font(ui_font(11.0)).color(muted()));
                        if ui.button(RichText::new("APPLY").font(mono(11.5))).clicked() {
                            pane.tab = StatsTab::Conditional;
                            reload = true;
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        for (label, line) in stats::presets() {
                            if ui.selectable_label(pane.condition.trim() == *line, *label).clicked() {
                                pane.condition = (*line).into();
                                reload = true;
                            }
                        }
                    });
                    ui.label(
                        RichText::new(
                            "AAPL COND RSI(14) < 30   ·   features look backward only; forward returns are the outcome",
                        )
                        .font(ui_font(11.0))
                        .color(muted()),
                    );
                    if let Some(snap) = &pane.snapshot {
                        conditional(ui, &snap.conditional);
                    }
                }
            }
        }

        if reload {
            if let PaneKind::Stats(pane) = &mut self.panes[i].kind {
                pane.request(id, &self.worker);
            }
        }
        if export {
            actions.push(Action::Command("EXPORT STATS".into()));
        }
        actions
    }
}

fn provenance_line(ui: &mut Ui, s: &StatsReport) {
    ui.label(
        RichText::new(format!(
            "{} · {} · as-of {} · {} … {} · {} daily bars",
            s.provenance.label(),
            s.source,
            s.as_of,
            s.start,
            s.end,
            s.bars
        ))
        .font(mono(11.5))
        .color(if s.provenance.is_verified() { muted() } else { theme::down() }),
    );
}

fn pct(v: Option<f64>, scale: f64) -> String {
    v.map(|x| format!("{:.2}%", x * scale)).unwrap_or_else(|| "—".into())
}

fn num(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.3}")).unwrap_or_else(|| "—".into())
}

fn overview(ui: &mut Ui, s: &StatsReport) {
    ui.add_space(4.0);
    ui.label(RichText::new("RISK / RETURN").font(mono(11.0)).color(muted()));
    ui.label(
        RichText::new(format!(
            "Ann. vol {vol}   Sharpe {sh}   Max DD {dd}   Avg daily {avg}   Win rate {win}   N={n} daily returns",
            vol = pct(s.metrics.volatility, 100.0),
            sh = num(s.metrics.sharpe),
            dd = pct(s.metrics.max_drawdown, 100.0),
            avg = pct(s.metrics.avg_daily_ret, 100.0),
            win = pct(s.metrics.win_rate, 100.0),
            n = s.metrics.n
        ))
        .font(mono(12.5))
        .color(text()),
    );
    ui.label(
        RichText::new(
            "Volatility and Sharpe annualise daily close-to-close returns with √252 and no risk-free rate. Max drawdown is the worst peak-to-trough on compounded daily returns. Win rate is the share of up days.",
        )
        .font(ui_font(11.0))
        .color(muted()),
    );

    ui.add_space(6.0);
    ui.label(RichText::new("DAILY RETURN DISTRIBUTION").font(mono(11.0)).color(muted()));
    hist_bars(ui, &s.distribution.iter().map(|b| (b.center, b.count)).collect::<Vec<_>>(), theme::up());

    ScrollArea::vertical().id_salt("stats-overview").auto_shrink([false, false]).show(ui, |ui| {
        section_bins(ui, "RSI(14) deciles → next-day return", &s.rsi_fwd_1d);
        section_bins(ui, "RSI(14) deciles → next-5-day return", &s.rsi_fwd_5d);
        section_bins(ui, "20-day vol deciles → next-day return", &s.vol_fwd_1d);
        for (period, bins) in &s.kama_distance_1d {
            section_bins(ui, &format!("KAMA({period}) distance deciles → next-day"), bins);
        }
        ui.add_space(4.0);
        ui.label(RichText::new("KAMA CROSSES (completed outcomes)").font(mono(11.0)).color(muted()));
        for c in &s.kama_crosses {
            ui.label(
                RichText::new(format!(
                    "{:<10}  1d {:>8} n={:<4}   5d {:>8} n={}",
                    c.label,
                    pct(c.fwd_1d, 100.0),
                    c.count_1d,
                    pct(c.fwd_5d, 100.0),
                    c.count_5d
                ))
                .font(mono(12.0))
                .color(text()),
            );
        }
        ui.add_space(4.0);
        ui.label(RichText::new("SEASONALITY (mean daily × 21 ≈ month)").font(mono(11.0)).color(muted()));
        let months = ["", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        for m in &s.seasonality {
            ui.label(
                RichText::new(format!(
                    "{:<4}  {:>8}   n={}",
                    months.get(m.month as usize).copied().unwrap_or("?"),
                    pct(m.value, 100.0),
                    m.count
                ))
                .font(mono(12.0))
                .color(text()),
            );
        }
    });
}

fn section_bins(ui: &mut Ui, title: &str, bins: &[tr_data::stats::BinMean]) {
    ui.add_space(4.0);
    ui.label(RichText::new(title.to_ascii_uppercase()).font(mono(11.0)).color(muted()));
    if bins.is_empty() {
        ui.label(RichText::new("Not enough distinct values to form deciles.").font(mono(12.0)).color(muted()));
        return;
    }
    for b in bins {
        ui.label(
            RichText::new(format!("D{:<2}  {:>8}   n={}", b.bin, pct(b.value, 100.0), b.count))
                .font(mono(12.0))
                .color(text()),
        );
    }
}

fn conditional(ui: &mut Ui, c: &ConditionalReport) {
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!(
            "Matched {} of {} bars ({:.1}%)  ·  {}",
            c.match_count,
            c.bars,
            c.match_rate * 100.0,
            c.condition_label
        ))
        .font(mono(12.5))
        .color(text()),
    );
    ui.label(RichText::new(&c.overlap_note).font(ui_font(11.0)).color(muted()));
    ScrollArea::vertical().id_salt("stats-cond").auto_shrink([false, false]).show(ui, |ui| {
        for h in &c.by_horizon {
            horizon_block(ui, h);
        }
    });
}

fn horizon_block(ui: &mut Ui, h: &HorizonCompare) {
    ui.add_space(8.0);
    ui.label(RichText::new(format!("{}-DAY FORWARD RETURN", h.horizon)).font(mono(11.0)).color(amber()));
    compare_row(ui, "Conditional", &h.conditional);
    compare_row(ui, "Baseline", &h.baseline);
    ui.label(
        RichText::new(format!(
            "Completed outcomes: cond N={} (effective {:.1})  ·  baseline N={} (effective {:.1})",
            h.conditional.count, h.conditional.effective_count, h.baseline.count, h.baseline.effective_count
        ))
        .font(mono(11.5))
        .color(muted()),
    );
    let pairs: Vec<(f64, usize)> = h.hist.centers.iter().copied().zip(h.hist.conditional.iter().copied()).collect();
    ui.label(RichText::new("Conditional histogram (shared bins with baseline)").font(ui_font(11.0)).color(muted()));
    hist_bars(ui, &pairs, theme::up());
    let base: Vec<(f64, usize)> = h.hist.centers.iter().copied().zip(h.hist.baseline.iter().copied()).collect();
    ui.label(RichText::new("Baseline histogram").font(ui_font(11.0)).color(muted()));
    hist_bars(ui, &base, muted());
}

fn compare_row(ui: &mut Ui, label: &str, r: &ReturnStats) {
    ui.label(
        RichText::new(format!(
            "{:<12} mean {:>8}  med {:>8}  std {:>8}  p05 {:>8}  p95 {:>8}  win {:>7}",
            label,
            pct(r.mean, 100.0),
            pct(r.median, 100.0),
            pct(r.std, 100.0),
            pct(r.p05, 100.0),
            pct(r.p95, 100.0),
            pct(r.win_rate, 100.0)
        ))
        .font(mono(12.0))
        .color(text()),
    );
}

fn hist_bars(ui: &mut Ui, bins: &[(f64, usize)], color: egui::Color32) {
    if bins.is_empty() {
        ui.label(RichText::new("No completed returns to plot.").font(mono(12.0)).color(muted()));
        return;
    }
    let max = bins.iter().map(|(_, c)| *c).max().unwrap_or(1).max(1) as f32;
    let height = dp(56.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(40.0), height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let n = bins.len() as f32;
    let w = rect.width() / n;
    for (i, (_, count)) in bins.iter().enumerate() {
        let h = (*count as f32 / max) * (height - 2.0);
        let x = rect.left() + i as f32 * w;
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x + 0.5, rect.bottom() - h), egui::pos2(x + w - 0.5, rect.bottom())),
            0.0,
            color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::command::{Command, Function};

    #[test]
    fn stats_commands_open_the_pane() {
        let mut app = test_app();
        app.apply(Function::Stats { condition: String::new(), conditional: false });
        assert!(app.panes.iter().any(|p| matches!(p.kind, PaneKind::Stats(_))));
        assert_eq!(
            crate::command::parse("STATS"),
            Some(Command::Function(Function::Stats { condition: String::new(), conditional: false }))
        );
        assert_eq!(
            crate::command::parse("AAPL STATS"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::Stats { condition: String::new(), conditional: false }
            ))
        );
        assert_eq!(
            crate::command::parse("AAPL COND RSI(14) < 30"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::Stats { condition: "RSI(14) < 30".into(), conditional: true }
            ))
        );
        assert_eq!(
            crate::command::parse("CDIST"),
            Some(Command::Function(Function::Stats { condition: String::new(), conditional: true }))
        );
    }

    #[test]
    fn stats_pane_rejects_sample_snapshots() {
        let mut pane = StatsPane::new("AAPL US Equity", "", false);
        pane.generation = 1;
        let mut snap = dummy_snap();
        snap.stats.provenance = tr_core::Provenance::Sample;
        pane.accept(1, Ok(snap));
        assert!(pane.snapshot.is_none());
        assert!(pane.error.as_deref().unwrap().contains("SAMPLE"));
    }

    fn dummy_snap() -> ResearchSnapshot {
        let ts = 1_700_000_000;
        let bars: Vec<tr_core::Bar> = (0..30)
            .map(|i| {
                let c = 10.0 + i as f64;
                tr_core::Bar::new(ts + i * 86_400, c, c, c, c, 1.0).with_provenance(tr_core::Provenance::Real)
            })
            .collect();
        stats::compute_snapshot("AAPL US Equity", "AAPL", &bars, "", &[5], "unit").unwrap()
    }
}
