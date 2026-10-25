//! Historical lookalike pane (`LOOKALIKE`, `KNN`): nearest past conditions.
use super::{PaneKind, TerminalApp};
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{RichText, ScrollArea, Sense, Ui};
use tr_data::lookalike::{self, HorizonSummary, LookalikeQuery, LookalikeReport};

pub struct LookalikePane {
    pub symbol: String,
    pub spec: String,
    pub as_of: String,
    pub k: String,
    pub gap: String,
    pub horizons: String,
    pub generation: u64,
    pub report: Option<LookalikeReport>,
    pub loading: bool,
    pub error: Option<String>,
    pub selected: Option<usize>,
}

impl LookalikePane {
    pub fn new(symbol: impl Into<String>, spec: impl Into<String>) -> Self {
        let spec = spec.into();
        let parsed = lookalike::parse_query(&spec).unwrap_or_default();
        Self {
            symbol: symbol.into(),
            spec: spec.clone(),
            as_of: parsed.as_of.unwrap_or_default(),
            k: parsed.k.to_string(),
            gap: parsed.gap.to_string(),
            horizons: parsed.horizons.iter().map(|h| h.to_string()).collect::<Vec<_>>().join(", "),
            generation: 0,
            report: None,
            loading: false,
            error: None,
            selected: None,
        }
    }

    pub fn query(&self) -> Result<LookalikeQuery, String> {
        let mut q = lookalike::parse_query(&self.spec)?;
        if let Ok(k) = self.k.trim().parse::<usize>()
            && k > 0
        {
            q.k = k;
        }
        if let Ok(gap) = self.gap.trim().parse::<usize>()
            && gap > 0
        {
            q.gap = gap;
        }
        if let Ok(hs) = lookalike::parse_horizons(self.horizons.trim()) {
            q.horizons = hs;
        }
        let as_of = self.as_of.trim();
        if !as_of.is_empty() {
            q.as_of = Some(tr_data::journal::parse_chart_date(as_of)?);
        }
        Ok(q)
    }

    pub fn accept(&mut self, generation: u64, result: Result<LookalikeReport, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(report) => {
                if let Err(e) = report.provenance.require_verified("Lookalikes") {
                    self.error = Some(e);
                    self.report = None;
                    return;
                }
                self.error = None;
                self.selected = None;
                self.as_of = report.as_of.clone();
                self.report = Some(report);
            }
            Err(e) => {
                self.error = Some(e);
                self.report = None;
            }
        }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        let symbol = self.symbol.trim();
        if symbol.is_empty() {
            self.error =
                Some("LOOKALIKE needs a ticker (AAPL LOOKALIKE). Sample/display prices are never used.".into());
            self.loading = false;
            return;
        }
        let query = match self.query() {
            Ok(q) => q,
            Err(e) => {
                self.error = Some(e);
                self.loading = false;
                return;
            }
        };
        self.generation += 1;
        self.loading = true;
        self.error = None;
        worker.send(Request::Lookalike {
            pane,
            generation: self.generation,
            symbol: tr_data::security(symbol).key(),
            query,
        });
    }
}

impl TerminalApp {
    pub(super) fn goto_chart_date(&mut self, symbol: Option<&str>, date: &str) {
        let date = match tr_data::journal::parse_chart_date(date.trim()) {
            Ok(d) => d,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let ts = match lookalike::date_to_ts(&date) {
            Ok(ts) => ts,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol());
        match symbol {
            Some(s) => self.open_archive_at(&s, ts),
            None => {
                if let Some(id) = self.watch_ui.active_chart.or_else(|| self.focused_visible())
                    && let Some(i) = self.idx(id)
                    && let PaneKind::Chart(c) = &mut self.panes[i].kind
                {
                    c.request_jump_to_ts(ts);
                    self.status = format!("{} · {}", c.symbol, date);
                    return;
                }
                self.status = "GOTO needs a ticker (AAPL GOTO 2024-01-15) or a focused chart".into();
            }
        }
    }

    pub(super) fn open_archive_at(&mut self, symbol: &str, ts: i64) {
        self.open_list_symbol(symbol, true, None);
        if let Some(id) = self.watch_ui.active_chart
            && let Some(i) = self.idx(id)
            && let PaneKind::Chart(c) = &mut self.panes[i].kind
        {
            c.request_jump_to_ts(ts);
        }
        let date = tr_data::journal::date_from_ts(ts).unwrap_or_else(|| ts.to_string());
        self.status = format!("{} · archive {date}", tr_data::security(symbol).key());
    }

    pub(super) fn open_lookalike(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        let mut pane = LookalikePane::new(symbol, spec);
        if pane.symbol.is_empty() {
            pane.error =
                Some("LOOKALIKE needs a ticker (AAPL LOOKALIKE). Sample/display prices are never used.".into());
        }
        let ticker = if pane.symbol.is_empty() { "Lookalikes".into() } else { tr_data::security(&pane.symbol).ticker };
        self.status = format!("{ticker}: historical lookalikes (verified archive only, paper / research)");
        self.open_panel(PaneKind::Lookalike(Box::new(pane)));
    }

    pub(super) fn lookalike_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let mut actions = Vec::new();
        let i = self.idx(id).expect("lookalike pane");
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        let mut reload = false;
        let mut export = false;
        let mut jump: Option<(String, i64)> = None;
        {
            let PaneKind::Lookalike(pane) = &mut self.panes[i].kind else { return vec![] };
            if !pane.loading && pane.report.is_none() && pane.error.is_none() && !pane.symbol.trim().is_empty() {
                pane.request(id, &self.worker);
            }

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Lookalikes").strong().color(text()));
                ui.label(RichText::new("paper / research only").font(mono(11.0)).color(amber()));
                if ui.small_button("Reload").on_hover_text("Recompute from the research archive").clicked() {
                    reload = true;
                }
                if ui
                    .small_button("Export")
                    .on_hover_text("Write lookalike JSON with source, as-of, counts and overlap (EXPORT LOOKALIKE)")
                    .clicked()
                {
                    export = true;
                }
            });
            ui.label(
                RichText::new(
                    "Find past sessions that looked like this date, jump the archive chart to a neighbor, and compare subsequent returns. Counts come before frequencies. Incomplete horizons are labelled, never invented.",
                )
                .font(ui_font(11.5))
                .color(muted()),
            );
            if sample_offline {
                ui.label(
                    RichText::new(
                        "Sample / offline display mode does not feed this pane. Lookalikes never fall back to sample bars.",
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
                ui.label(RichText::new("As-of").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.as_of).desired_width(92.0).hint_text("YYYY-MM-DD"));
                ui.label(RichText::new("K").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.k).desired_width(36.0).hint_text("20"));
                ui.label(RichText::new("Gap").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.gap).desired_width(36.0).hint_text("21"));
                ui.label(RichText::new("Horizons").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.horizons).desired_width(80.0).hint_text("5, 10, 21"));
                if ui.button(RichText::new("RUN").font(mono(11.5))).clicked() {
                    reload = true;
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (label, spec) in [
                    ("K=10", "K=10"),
                    ("K=20", "K=20"),
                    ("Gap 21", "GAP=21"),
                    ("H 5,10,21", "H=5,10,21"),
                    ("H 5,21", "H=5,21"),
                ] {
                    if ui.small_button(label).clicked() {
                        if let Ok(q) = lookalike::parse_query(spec) {
                            if spec.starts_with('K') {
                                pane.k = q.k.to_string();
                            } else if spec.starts_with("GAP") {
                                pane.gap = q.gap.to_string();
                            } else {
                                pane.horizons = q.horizons.iter().map(|h| h.to_string()).collect::<Vec<_>>().join(", ");
                            }
                        }
                        reload = true;
                    }
                }
            });

            if let Some(err) = &pane.error {
                ui.label(RichText::new(err).font(mono(12.0)).color(theme::down()));
            } else if pane.loading {
                ui.label(RichText::new("Reading research archive…").font(mono(12.0)).color(muted()));
            }

            if pane.report.is_some() {
                jump = render_report(ui, pane);
            }
        }

        if reload {
            if let PaneKind::Lookalike(pane) = &mut self.panes[i].kind {
                pane.request(id, &self.worker);
            }
        }
        if export {
            actions.push(Action::Command("EXPORT LOOKALIKE".into()));
        }
        if let Some((symbol, ts)) = jump {
            actions.push(Action::OpenArchiveAt { symbol, ts });
        }
        actions
    }
}

fn render_report(ui: &mut Ui, pane: &mut LookalikePane) -> Option<(String, i64)> {
    let report = pane.report.clone().expect("lookalike report");
    let mut jump = None;
    ui.label(
        RichText::new(format!(
            "{} · {} · as-of {} · {} … {} · {} daily bars",
            report.provenance.label(),
            report.source,
            report.as_of,
            report.start,
            report.end,
            report.bars
        ))
        .font(mono(11.5))
        .color(if report.provenance.is_verified() { muted() } else { theme::down() }),
    );
    ui.label(
        RichText::new(format!(
            "Query {}  ·  distance {}  ·  requested K={}  ·  selected N={}  ·  discarded overlap {}  ·  eligible {}",
            report.query.date,
            report.distance,
            report.requested_k,
            report.selected_n,
            report.discarded_overlap,
            report.eligible_n
        ))
        .font(mono(11.5))
        .color(text()),
    );
    ui.label(RichText::new(&report.feature_note).font(ui_font(11.0)).color(muted()));
    ui.label(RichText::new(&report.overlap_note).font(ui_font(11.0)).color(amber()));

    ui.add_space(6.0);
    ui.label(RichText::new("PER-HORIZON COUNTS (before frequencies)").font(mono(11.0)).color(muted()));
    ui.label(
        RichText::new(format!(
            "{:<6} {:>8} {:>10} {:>11} {:>10} {:>8} {:>8} {:>8}",
            "H", "req N", "completed", "incomplete", "eff. N", "mean", "win%", "p25/p75"
        ))
        .font(mono(11.0))
        .color(muted()),
    );
    for h in &report.by_horizon {
        count_row(ui, h);
    }
    for h in &report.by_horizon {
        ui.label(RichText::new(format!("  H={}: {}", h.horizon, h.overlap_note)).font(mono(10.5)).color(muted()));
    }

    ui.add_space(6.0);
    ui.label(
        RichText::new("NEIGHBORS (click a row or press Enter to jump the archive chart)")
            .font(mono(11.0))
            .color(muted()),
    );
    let header = format!(
        "{:<12} {:>8} {}",
        "Date",
        "dist",
        report.by_horizon.iter().map(|h| format!(" {:>10}", format!("{}d", h.horizon))).collect::<String>()
    );
    ui.label(RichText::new(header).font(mono(11.0)).color(muted()));

    ScrollArea::vertical().id_salt("lookalike-neighbors").auto_shrink([false, false]).show(ui, |ui| {
        for (idx, n) in report.neighbors.iter().enumerate() {
            let mut cols = String::new();
            for h in &report.by_horizon {
                let o = n.outcomes.iter().find(|o| o.horizon == h.horizon);
                cols.push_str(&format!(
                    " {:>10}",
                    match o {
                        Some(o) if o.completed => o.ret.map(|r| format!("{:+.1}%", r * 100.0)).unwrap_or_else(|| "—".into()),
                        Some(o) => o.reason.as_deref().map(|_| "incomp.".into()).unwrap_or_else(|| "incomp.".into()),
                        None => "—".into(),
                    }
                ));
            }
            let line = format!("{:<12} {:>8.3}{cols}", n.date, n.distance);
            let selected = pane.selected == Some(idx);
            let resp = ui.add(
                egui::Label::new(RichText::new(line).font(mono(12.0)).color(if selected { amber() } else { text() }))
                    .sense(Sense::click()),
            );
            if resp.hovered() {
                if let Some(o) = n.outcomes.iter().find(|o| !o.completed) {
                    resp.clone().on_hover_text(o.reason.clone().unwrap_or_else(|| "incomplete horizon".into()));
                } else {
                    resp.clone().on_hover_text(format!("Jump archive chart to {}", n.date));
                }
            }
            if resp.clicked() {
                pane.selected = Some(idx);
                jump = Some((report.symbol.clone(), n.ts));
            }
        }
        if ui.input(|i| i.key_pressed(egui::Key::Enter))
            && let Some(idx) = pane.selected
            && let Some(n) = report.neighbors.get(idx)
        {
            jump = Some((report.symbol.clone(), n.ts));
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "AAPL LOOKALIKE   ·   AAPL KNN 2024-01-15 K=15   ·   GOTO 2024-01-15   ·   EXPORT LOOKALIKE   ·   see docs/LOOKALIKE.md",
            )
            .font(ui_font(11.0))
            .color(muted()),
        );
    });
    jump
}

fn count_row(ui: &mut Ui, h: &HorizonSummary) {
    ui.label(
        RichText::new(format!(
            "{:<6} {:>8} {:>10} {:>11} {:>10.2} {:>8} {:>8} {:>8}",
            h.horizon,
            h.requested_n,
            h.completed_n,
            h.incomplete_n,
            h.effective_n,
            pct(h.mean),
            pct(h.win_rate),
            format!("{}/{}", pct(h.p25), pct(h.p75))
        ))
        .font(mono(12.0))
        .color(text()),
    );
}

fn pct(v: Option<f64>) -> String {
    v.map(|x| format!("{:.1}%", x * 100.0)).unwrap_or_else(|| "—".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::command::{Command, Function};

    #[test]
    fn lookalike_commands_open_the_pane() {
        let mut app = test_app();
        app.apply(Function::Lookalike { spec: String::new() });
        assert!(app.panes.iter().any(|p| matches!(p.kind, PaneKind::Lookalike(_))));
        assert_eq!(
            crate::command::parse("LOOKALIKE"),
            Some(Command::Function(Function::Lookalike { spec: String::new() }))
        );
        assert_eq!(
            crate::command::parse("AAPL LOOKALIKE"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Lookalike { spec: String::new() }))
        );
        assert_eq!(
            crate::command::parse("AAPL KNN 2024-01-15 K=15"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::Lookalike { spec: "2024-01-15 K=15".into() }
            ))
        );
        assert_eq!(
            crate::command::parse("GOTO 2024-01-15"),
            Some(Command::Function(Function::Goto { date: "2024-01-15".into() }))
        );
        assert!(lookalike::parse_query("K=15 GAP=21 H=5,10,21").is_ok());
    }

    #[test]
    fn lookalike_aliases_parse() {
        for code in ["LOOKALIKE", "KNN", "ANALOG", "ANALOGUE", "NN"] {
            assert!(
                matches!(crate::command::parse(code), Some(Command::Function(Function::Lookalike { .. }))),
                "{code}"
            );
        }
    }
}
