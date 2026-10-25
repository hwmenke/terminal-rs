//! Trade journal pane: save a setup (date, reasoning, planned risk, checklist),
//! record the outcome in dollars and R-multiples, and attach notes to chart dates.

use egui::{RichText, ScrollArea, Sense, Ui};
use tr_core::Provenance;
use tr_data::journal::{self, DateNote, Setup, SetupDraft, Store};

use super::{PaneKind, TerminalApp};
use crate::congress_view::Action;
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::worker::data_dir;

pub struct JournalPane {
    pub filter: String,
    pub selected: Option<String>,
    pub draft: SetupDraft,
    pub outcome_dollars: String,
    pub outcome_r: String,
    pub outcome_note: String,
    pub note_date: String,
    pub note_text: String,
    pub tab: JournalTab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalTab {
    Setups,
    Notes,
}

impl JournalPane {
    pub fn new(filter: impl Into<String>) -> Self {
        Self {
            filter: filter.into(),
            selected: None,
            draft: SetupDraft::default(),
            outcome_dollars: String::new(),
            outcome_r: String::new(),
            outcome_note: String::new(),
            note_date: String::new(),
            note_text: String::new(),
            tab: JournalTab::Setups,
        }
    }

    fn prefill(&mut self, symbol: Option<&str>, date: Option<&str>, store: &Store) {
        if let Some(s) = symbol.filter(|s| !s.is_empty()) {
            let key = tr_data::security(s).key();
            self.filter = key.clone();
            self.draft.symbol = tr_data::security(&key).ticker;
        }
        if let Some(d) = date.filter(|d| journal::parse_chart_date(d).is_ok()) {
            self.draft.chart_date = d.to_string();
            self.note_date = d.to_string();
        }
        if self.draft.checklist.is_empty() {
            self.draft.checklist = store.blank_checklist();
        }
    }
}

impl TerminalApp {
    pub(super) fn journal_path(&self) -> std::path::PathBuf {
        self.store.dir.parent().map(journal::store_path).unwrap_or_else(|| journal::store_path(data_dir()))
    }

    pub(super) fn persist_journal(&mut self) {
        self.journal.save(&self.journal_path());
        self.sync_chart_journal();
    }

    pub(super) fn open_journal(&mut self, symbol: Option<&str>, extra: &str) {
        let extra = extra.trim();
        let date = journal::parse_chart_date(extra).ok();
        let filter = symbol
            .map(|s| tr_data::security(s).key())
            .or_else(|| if extra.is_empty() || date.is_some() { None } else { Some(tr_data::security(extra).key()) })
            .unwrap_or_default();
        let mut pane = JournalPane::new(filter);
        pane.prefill(symbol.or((date.is_none() && !extra.is_empty()).then_some(extra)), date.as_deref(), &self.journal);
        if pane.draft.symbol.is_empty() {
            if let Some(s) = self.focused_symbol() {
                pane.draft.symbol = tr_data::security(&s).ticker;
            }
        }
        if pane.draft.chart_date.is_empty() {
            pane.draft.chart_date = self.focused_chart_date().unwrap_or_default();
        }
        if pane.draft.checklist.is_empty() {
            pane.draft.checklist = self.journal.blank_checklist();
        }
        if let Some(risk) = self.focused_desk_risk() {
            pane.draft.planned_risk = risk;
        }
        if extra.is_empty() && date.is_none() {
            pane.draft.chart_provenance = self.focused_chart_provenance();
        }
        let status = if pane.filter.is_empty() {
            "Journal: save a setup, tick the checklist, then record $ and R".into()
        } else {
            format!("{}: trade journal", tr_data::security(&pane.filter).ticker)
        };
        self.open_panel(PaneKind::Journal(Box::new(pane)));
        self.status = status;
    }

    pub(super) fn add_journal_note(&mut self, symbol: Option<&str>, date: &str, text: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        let date =
            if date.trim().is_empty() { self.focused_chart_date().unwrap_or_default() } else { date.to_string() };
        match self.journal.add_note(&symbol, &date, text, now_ts()) {
            Ok(n) => {
                self.persist_journal();
                self.open_journal(Some(&n.symbol), &n.chart_date);
                for pane in &mut self.panes {
                    if let PaneKind::Journal(j) = &mut pane.kind {
                        j.tab = JournalTab::Notes;
                    }
                }
                self.status = format!("{} {}: note saved", n.ticker(), n.chart_date);
            }
            Err(e) => {
                self.status = e;
                self.open_journal((!symbol.is_empty()).then_some(symbol.as_str()), &date);
            }
        }
    }

    fn focused_chart_date(&self) -> Option<String> {
        self.peek_chart().and_then(|c| c.journal_date())
    }

    fn focused_chart_provenance(&self) -> Provenance {
        self.peek_chart().map(|c| c.provenance()).unwrap_or(Provenance::Missing)
    }

    fn focused_desk_risk(&self) -> Option<f64> {
        self.panes.iter().find_map(|p| match &p.kind {
            PaneKind::Desk(d) if d.dollar_risk.is_finite() && d.dollar_risk > 0.0 => Some(d.dollar_risk),
            _ => None,
        })
    }

    pub(super) fn journal_pane(&mut self, ui: &mut Ui, id: crate::dock::PaneId) -> Vec<Action> {
        let i = match self.idx(id) {
            Some(i) => i,
            None => return vec![],
        };
        let mut actions = vec![];
        let mut persist = false;
        let mut status = None;
        let mut save_setup = false;
        let mut record = None;
        let mut clear_out = None;
        let mut drop_setup = None;
        let mut save_note = false;
        let mut drop_note = None;
        let open = self.journal.open_count();
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        let focused_ticker = self.focused_symbol().map(|s| tr_data::security(&s).ticker);
        let focused_date = self.focused_chart_date();
        let blank = self.journal.blank_checklist();
        let filter = match &self.panes[i].kind {
            PaneKind::Journal(p) => p.filter.clone(),
            _ => return vec![],
        };
        let setups: Vec<Setup> = self.journal.setups_for(&filter).into_iter().cloned().collect();
        let notes: Vec<DateNote> = self.journal.notes_for(&filter).into_iter().cloned().collect();

        {
            let PaneKind::Journal(pane) = &mut self.panes[i].kind else { return vec![] };
            if pane.draft.checklist.is_empty() {
                pane.draft.checklist = blank;
            }
            if pane.draft.symbol.is_empty() {
                if let Some(s) = focused_ticker {
                    pane.draft.symbol = s;
                }
            }
            if pane.draft.chart_date.is_empty() {
                if let Some(d) = focused_date {
                    pane.draft.chart_date = d;
                }
            }

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Trade journal").strong().color(text()));
                for (tab, label) in [(JournalTab::Setups, "Setups"), (JournalTab::Notes, "Date notes")] {
                    if ui.selectable_label(pane.tab == tab, label).clicked() {
                        pane.tab = tab;
                    }
                }
                ui.label(RichText::new(format!("{open} open")).font(mono(12.0)).color(if open > 0 {
                    amber()
                } else {
                    muted()
                }));
                if ui
                    .small_button("Export")
                    .on_hover_text("Write journal JSON with dates and provenance (EXPORT JOURNAL)")
                    .clicked()
                {
                    actions.push(Action::Command("EXPORT JOURNAL".into()));
                }
            });
            ui.label(
                RichText::new(
                    "Save a setup with its chart date, reasoning, planned $ risk and checklist. Record the outcome in dollars and R (multiples of that risk). Notes attach to historical dates. Sample bars never invent P&L.",
                )
                .font(ui_font(11.5))
                .color(muted()),
            );
            if sample_offline {
                ui.label(
                    RichText::new(
                        "Sample / offline mode: journal rows are user-authored only. Outcomes are never computed from sample prices.",
                    )
                    .font(ui_font(12.0))
                    .color(theme::down()),
                );
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Filter").color(muted()));
                ui.add(egui::TextEdit::singleline(&mut pane.filter).desired_width(90.0).hint_text("ticker"));
            });

            match pane.tab {
                JournalTab::Setups => {
                    setup_form(ui, pane, &mut save_setup);
                    ui.add_space(4.0);
                    ui.label(RichText::new("SETUPS").font(mono(11.0)).color(muted()));
                    if setups.is_empty() {
                        ui.label(
                            RichText::new("No setups yet. JOURNAL / AAPL JOURNAL, or Save to journal from DESK.")
                                .font(mono(12.0))
                                .color(muted()),
                        );
                    } else {
                        ScrollArea::vertical()
                            .id_salt("journal-setups")
                            .max_height(260.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for s in &setups {
                                    setup_row(ui, pane, s, &mut actions, &mut drop_setup, &mut record, &mut clear_out);
                                }
                            });
                        if let Some(sel) = pane.selected.clone()
                            && let Some(s) = setups.iter().find(|x| x.id == sel)
                        {
                            outcome_form(ui, pane, s, &mut record);
                        }
                    }
                }
                JournalTab::Notes => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Date").color(muted()));
                        ui.add(
                            egui::TextEdit::singleline(&mut pane.note_date).desired_width(92.0).hint_text("YYYY-MM-DD"),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut pane.note_text)
                                .desired_width(280.0)
                                .hint_text("note for this chart date"),
                        );
                        if ui.button(RichText::new("SAVE NOTE").font(mono(11.5))).clicked() {
                            save_note = true;
                        }
                    });
                    ui.label(
                        RichText::new(
                            "AAPL NOTE 2026-10-24 wait for the 20-day  ·  notes stay on that historical date",
                        )
                        .font(ui_font(11.0))
                        .color(muted()),
                    );
                    ui.add_space(4.0);
                    ui.label(RichText::new("NOTES").font(mono(11.0)).color(muted()));
                    if notes.is_empty() {
                        ui.label(RichText::new("No date notes yet.").font(mono(12.0)).color(muted()));
                    } else {
                        ScrollArea::vertical().id_salt("journal-notes").auto_shrink([false, false]).show(ui, |ui| {
                            for n in &notes {
                                note_row(ui, n, &mut actions, &mut drop_note);
                            }
                        });
                    }
                }
            }
        }

        if save_setup {
            if let PaneKind::Journal(pane) = &mut self.panes[i].kind {
                match self.journal.add_setup(pane.draft.clone(), now_ts()) {
                    Ok(s) => {
                        pane.selected = Some(s.id.clone());
                        pane.draft.reasoning.clear();
                        pane.draft.checklist = self.journal.blank_checklist();
                        persist = true;
                        status =
                            Some(format!("{} {}: setup saved (1R = ${:.2})", s.ticker(), s.chart_date, s.planned_risk));
                    }
                    Err(e) => status = Some(e),
                }
            }
        }
        if let Some(id) = record
            && let PaneKind::Journal(pane) = &self.panes[i].kind
        {
            let dollars = parse_opt_f64(&pane.outcome_dollars);
            let r = parse_opt_f64(&pane.outcome_r);
            match (dollars, r) {
                (Err(e), _) | (_, Err(e)) => status = Some(e),
                (Ok(d), Ok(rm)) => match self.journal.record_outcome(&id, d, rm, &pane.outcome_note, now_ts()) {
                    Ok(out) => {
                        persist = true;
                        status = Some(format!("Outcome ${:.2} · {:.2}R", out.dollars, out.r_multiple));
                    }
                    Err(e) => status = Some(e),
                },
            }
        }
        if let Some(id) = clear_out {
            if self.journal.clear_outcome(&id) {
                persist = true;
                status = Some("Outcome cleared".into());
            }
        }
        if let Some(id) = drop_setup {
            if self.journal.remove_setup(&id) {
                persist = true;
                status = Some("Setup removed".into());
            }
        }
        if save_note {
            if let PaneKind::Journal(pane) = &self.panes[i].kind {
                let symbol =
                    if pane.filter.trim().is_empty() { pane.draft.symbol.clone() } else { pane.filter.clone() };
                match self.journal.add_note(&symbol, &pane.note_date, &pane.note_text, now_ts()) {
                    Ok(n) => {
                        persist = true;
                        status = Some(format!("{} {}: note saved", n.ticker(), n.chart_date));
                    }
                    Err(e) => status = Some(e),
                }
            }
        }
        if let Some(id) = drop_note {
            if self.journal.remove_note(&id) {
                persist = true;
                status = Some("Note removed".into());
            }
        }
        if persist {
            self.persist_journal();
        }
        if let Some(s) = status {
            self.status = s;
        }
        actions
    }

    pub(super) fn sync_chart_journal(&mut self) {
        for pane in &mut self.panes {
            if let PaneKind::Chart(c) = &mut pane.kind {
                c.set_journal_marks(self.journal.marked_dates(&c.symbol));
            }
        }
    }
}

fn setup_form(ui: &mut Ui, pane: &mut JournalPane, save: &mut bool) {
    ui.add_space(4.0);
    ui.label(RichText::new("NEW SETUP").font(mono(11.0)).color(muted()));
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Ticker").color(muted()));
        ui.add(egui::TextEdit::singleline(&mut pane.draft.symbol).desired_width(72.0).hint_text("AAPL"));
        ui.label(RichText::new("Chart date").color(muted()));
        ui.add(egui::TextEdit::singleline(&mut pane.draft.chart_date).desired_width(92.0).hint_text("YYYY-MM-DD"));
        ui.label(RichText::new("$ risk").color(muted()));
        ui.add(
            egui::DragValue::new(&mut pane.draft.planned_risk)
                .speed(5.0)
                .range(0.01..=1_000_000.0)
                .prefix("$")
                .max_decimals(2),
        );
    });
    ui.add(
        egui::TextEdit::multiline(&mut pane.draft.reasoning)
            .desired_width(f32::INFINITY)
            .desired_rows(2)
            .hint_text("Reasoning: why this date, what has to hold, what invalidates"),
    );
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Checklist").color(muted()));
        for item in &mut pane.draft.checklist {
            ui.checkbox(&mut item.checked, item.label.as_str());
        }
    });
    if pane.draft.chart_provenance == Provenance::Sample {
        ui.label(
            RichText::new("Chart date taken from SAMPLE DATA — outcome will still be typed, never computed.")
                .font(ui_font(11.5))
                .color(theme::down()),
        );
    }
    ui.horizontal_wrapped(|ui| {
        if ui.button(RichText::new("SAVE SETUP").font(mono(11.5))).clicked() {
            *save = true;
        }
        ui.label(
            RichText::new("AAPL JOURNAL 2026-10-24  ·  planned $ risk is 1R for the outcome")
                .font(ui_font(11.0))
                .color(muted()),
        );
    });
}

fn setup_row(
    ui: &mut Ui,
    pane: &mut JournalPane,
    s: &Setup,
    actions: &mut Vec<Action>,
    drop_setup: &mut Option<String>,
    record: &mut Option<String>,
    clear_out: &mut Option<String>,
) {
    let selected = pane.selected.as_deref() == Some(s.id.as_str());
    let ticker = s.ticker();
    let result = match &s.outcome {
        Some(o) => format!("  ${:.0}  {:+.2}R", o.dollars, o.r_multiple),
        None => "  open".into(),
    };
    let checks = s.checklist.iter().filter(|c| c.checked).count();
    let line = format!(
        "{:<8} {}  ${:<7.0}  {checks}/{}  {}{}",
        ticker,
        s.chart_date,
        s.planned_risk,
        s.checklist.len(),
        if s.reasoning.chars().count() > 48 {
            format!("{}…", s.reasoning.chars().take(48).collect::<String>())
        } else {
            s.reasoning.clone()
        },
        result
    );
    let color = if s.chart_provenance == Provenance::Sample {
        theme::down()
    } else if selected {
        amber()
    } else if s.open() {
        text()
    } else {
        muted()
    };
    ui.horizontal(|ui| {
        let resp = ui.add(egui::Label::new(RichText::new(line).font(mono(12.0)).color(color)).sense(Sense::click()));
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() {
            pane.selected = Some(s.id.clone());
            actions.push(Action::OpenChart(s.symbol.clone()));
        }
        if ui.small_button("Chart").clicked() {
            actions.push(Action::OpenChart(s.symbol.clone()));
        }
        if s.open() {
            if ui.small_button("Record").clicked() {
                pane.selected = Some(s.id.clone());
                *record = None;
            }
        } else if ui.small_button("Clear result").clicked() {
            *clear_out = Some(s.id.clone());
        }
        if ui.small_button("Remove").clicked() {
            *drop_setup = Some(s.id.clone());
        }
    });
}

fn outcome_form(ui: &mut Ui, pane: &mut JournalPane, s: &Setup, record: &mut Option<String>) {
    ui.add_space(6.0);
    ui.separator();
    ui.label(
        RichText::new(format!(
            "Outcome for {} {} · 1R = ${:.2}{}",
            s.ticker(),
            s.chart_date,
            s.planned_risk,
            s.outcome
                .as_ref()
                .map(|o| format!(" · last ${:.2} ({:+.2}R)", o.dollars, o.r_multiple))
                .unwrap_or_default()
        ))
        .color(text()),
    );
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("$ P&L").color(muted()));
        ui.add(egui::TextEdit::singleline(&mut pane.outcome_dollars).desired_width(72.0).hint_text("500"));
        ui.label(RichText::new("or R").color(muted()));
        ui.add(egui::TextEdit::singleline(&mut pane.outcome_r).desired_width(56.0).hint_text("2"));
        ui.add(egui::TextEdit::singleline(&mut pane.outcome_note).desired_width(180.0).hint_text("optional note"));
        if ui.button(RichText::new("RECORD").font(mono(11.5))).clicked() {
            *record = Some(s.id.clone());
        }
    });
    ui.label(
        RichText::new("Enter dollars or R; the other is derived from planned risk. Never from the price feed.")
            .font(ui_font(11.0))
            .color(muted()),
    );
}

fn note_row(ui: &mut Ui, n: &DateNote, actions: &mut Vec<Action>, drop_note: &mut Option<String>) {
    ui.horizontal(|ui| {
        let line = format!("{:<8} {}  {}", n.ticker(), n.chart_date, n.text);
        let resp = ui.add(egui::Label::new(RichText::new(line).font(mono(12.0)).color(text())).sense(Sense::click()));
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() {
            actions.push(Action::OpenChart(n.symbol.clone()));
        }
        if ui.small_button("Remove").clicked() {
            *drop_note = Some(n.id.clone());
        }
    });
}

fn parse_opt_f64(s: &str) -> Result<Option<f64>, String> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let t = t.trim_end_matches(['r', 'R']);
    t.parse::<f64>().ok().filter(|v| v.is_finite()).map(Some).ok_or_else(|| format!("not a number: {s}"))
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::command::{Command, Function};

    #[test]
    fn journal_commands_open_a_pane_and_save_a_setup() {
        let mut app = test_app();
        app.apply(Function::Journal(String::new()));
        assert!(app.panes.iter().any(|p| matches!(p.kind, PaneKind::Journal(_))));
        assert_eq!(crate::command::parse("JOURNAL"), Some(Command::Function(Function::Journal(String::new()))));
        assert_eq!(
            crate::command::parse("AAPL JOURNAL"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Journal(String::new())))
        );

        let i = app.panes.iter().position(|p| matches!(p.kind, PaneKind::Journal(_))).unwrap();
        if let PaneKind::Journal(j) = &mut app.panes[i].kind {
            j.draft.symbol = "AAPL".into();
            j.draft.chart_date = "2026-10-24".into();
            j.draft.reasoning = "box breakout".into();
            j.draft.planned_risk = 200.0;
            j.draft.checklist = journal::default_checklist();
            j.draft.checklist[0].checked = true;
        }
        if let PaneKind::Journal(pane) = &app.panes[i].kind {
            app.journal.add_setup(pane.draft.clone(), 1).unwrap();
        }
        assert_eq!(app.journal.setups.len(), 1);
        assert_eq!(app.journal.setups[0].chart_date, "2026-10-24");
        assert_eq!(app.journal.setups[0].planned_risk, 200.0);
    }

    #[test]
    fn note_command_attaches_to_a_historical_date() {
        let mut app = test_app();
        app.dispatch(Command::SymbolFunction(
            "AAPL US Equity".into(),
            Function::JournalNote { date: "2026-03-15".into(), text: "wait for 20-day".into() },
        ));
        assert_eq!(app.journal.notes.len(), 1);
        assert_eq!(app.journal.notes[0].chart_date, "2026-03-15");
        assert!(app.journal.notes[0].text.contains("20-day"));
        assert!(app.panes.iter().any(|p| matches!(p.kind, PaneKind::Journal(_))));
    }

    #[test]
    fn sample_provider_does_not_invent_an_outcome() {
        let mut app = test_app();
        assert!(app.provider.eq_ignore_ascii_case("sample"));
        let setup = app
            .journal
            .add_setup(
                SetupDraft {
                    symbol: "AAPL".into(),
                    chart_date: "2026-10-24".into(),
                    reasoning: "demo only".into(),
                    planned_risk: 100.0,
                    checklist: journal::default_checklist(),
                    chart_provenance: Provenance::Sample,
                },
                1,
            )
            .unwrap();
        assert_eq!(setup.chart_provenance, Provenance::Sample);
        assert!(setup.open());
        // No quote/bar helper exists; recording stays explicit.
        let out = app.journal.record_outcome(&setup.id, Some(150.0), None, "", 2).unwrap();
        assert!((out.r_multiple - 1.5).abs() < 1e-9);
    }
}
