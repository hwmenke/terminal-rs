//! Session brief (`BRIEF`): path, pace, gap, extreme, and versus SPY on one name.
use super::{PaneKind, TerminalApp};
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{RichText, Ui};
use tr_data::session_alerts::SessionSignal;
use tr_data::session_brief::SessionBrief;

pub struct SessionBriefPane {
    pub symbol: String,
    pub generation: u64,
    pub brief: Option<SessionBrief>,
    pub loading: bool,
    pub error: Option<String>,
}

impl SessionBriefPane {
    pub fn new(symbol: impl Into<String>) -> Self {
        let symbol = symbol.into();
        let symbol = if symbol.trim().is_empty() { symbol } else { tr_data::security(&symbol).key() };
        Self { symbol, generation: 0, brief: None, loading: false, error: None }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        let symbol = self.symbol.trim();
        if symbol.is_empty() {
            self.error = Some("BRIEF needs a ticker (AAPL BRIEF). Sample prices are never used.".into());
            self.loading = false;
            return;
        }
        self.generation += 1;
        self.loading = true;
        self.error = None;
        self.brief = None;
        worker.send(Request::SessionBrief {
            pane,
            generation: self.generation,
            symbol: tr_data::security(symbol).key(),
        });
    }

    pub fn accept(&mut self, generation: u64, result: Result<SessionBrief, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(brief) if !brief.provenance.is_verified() => {
                self.brief = None;
                self.error = Some(format!(
                    "Session brief refused: {}. Verified real data is required.",
                    brief.provenance.label()
                ));
            }
            Ok(brief) => {
                self.brief = Some(brief);
                self.error = None;
            }
            Err(err) => {
                self.brief = None;
                self.error = Some(err);
            }
        }
    }
}

impl TerminalApp {
    pub(super) fn open_session_brief(&mut self, symbol: Option<&str>) {
        let symbol = symbol
            .map(|s| tr_data::security(s).key())
            .filter(|s| !s.is_empty())
            .or_else(|| self.focused_symbol())
            .unwrap_or_else(|| tr_data::security("AAPL").key());
        let pane = SessionBriefPane::new(symbol);
        let ticker = tr_data::security(&pane.symbol).ticker;
        self.status = format!("{ticker}: session brief — path, pace, gap, extreme, and versus SPY");
        self.open_panel(PaneKind::SessionBrief(Box::new(pane)));
    }

    pub(super) fn session_brief_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("session brief pane");
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        let mut reload = false;
        {
            let PaneKind::SessionBrief(pane) = &mut self.panes[i].kind else { return vec![] };
            if !pane.loading && pane.brief.is_none() && pane.error.is_none() && !pane.symbol.trim().is_empty() {
                pane.request(id, &self.worker);
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Session brief").strong().color(text()));
                ui.label(RichText::new("five reads, not a chart").font(mono(11.0)).color(amber()));
                if ui.small_button("Reload").on_hover_text("Read verified 5-minute bars again").clicked() {
                    reload = true;
                }
            });
            ui.label(
                RichText::new(
                    "Path, pace, gap, extreme, and versus SPY, each on its usual window of verified 5-minute bars. A line that is not ready stays on the brief. Sample prices are never used.",
                )
                .font(ui_font(11.5))
                .color(muted()),
            );
            if sample_offline {
                ui.label(
                    RichText::new(
                        "Sample / offline display mode does not feed this pane. The brief never falls back to sample bars.",
                    )
                    .font(ui_font(12.0))
                    .color(theme::down()),
                );
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Ticker").color(muted()));
                let changed = ui
                    .add(egui::TextEdit::singleline(&mut pane.symbol).desired_width(160.0).hint_text("AAPL"))
                    .lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if changed || ui.button(RichText::new("RUN").font(mono(11.5))).clicked() {
                    reload = true;
                }
            });
            if let Some(err) = &pane.error {
                ui.label(RichText::new(err).font(mono(12.0)).color(theme::down()));
            } else if pane.loading {
                ui.label(RichText::new("Reading verified intraday bars…").font(mono(12.0)).color(muted()));
            }
            if let Some(brief) = &pane.brief {
                render_brief(ui, brief);
            }
        }
        if reload && let PaneKind::SessionBrief(pane) = &mut self.panes[i].kind {
            pane.request(id, &self.worker);
        }
        Vec::new()
    }
}

fn render_brief(ui: &mut Ui, brief: &SessionBrief) {
    let when = brief.date.map(|d| d.to_string()).unwrap_or_else(|| "no session".into());
    let stale = !brief.fresh;
    ui.label(
        RichText::new(format!(
            "{} · {} · 5-minute · {}{}",
            brief.ticker,
            when,
            brief.provenance.label(),
            if stale { " · last bar is not current" } else { "" }
        ))
        .font(mono(11.5))
        .color(if brief.provenance.is_verified() { muted() } else { theme::down() }),
    );
    for line in &brief.lines {
        let color = match line.active {
            Some(true) => theme::up(),
            Some(false) => muted(),
            None => theme::down(),
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(heading(line.signal)).font(mono(12.0)).color(text()));
            ui.label(RichText::new(line.mark()).font(mono(12.0)).strong().color(color));
            ui.label(RichText::new(&line.detail).font(ui_font(12.0)).color(text()));
        });
    }
}

fn heading(signal: SessionSignal) -> &'static str {
    match signal {
        SessionSignal::Path => "Path",
        SessionSignal::Pace => "Pace",
        SessionSignal::Gap => "Gap",
        SessionSignal::Extreme => "Extreme",
        SessionSignal::Relative => "vs SPY",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::command::{Command, Function};
    use tr_core::Provenance;

    #[test]
    fn brief_commands_open_the_pane_and_request_one_read() {
        let mut app = test_app();
        app.apply(Function::SessionBrief);
        let pane = app
            .panes
            .iter()
            .find_map(|pane| match &pane.kind {
                PaneKind::SessionBrief(pane) => Some(pane),
                _ => None,
            })
            .expect("session brief");
        assert_eq!(pane.symbol, "AAPL US Equity");
        assert!(app.status.contains("session brief"));
        assert_eq!(parse_brief("BRIEF"), Some(Command::Function(Function::SessionBrief)));
        assert_eq!(parse_brief("SBRIEF"), Some(Command::Function(Function::SessionBrief)));
        assert_eq!(
            parse_brief("AAPL BRIEF"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::SessionBrief))
        );
        assert_eq!(
            parse_brief("MSFT SBRIEF"),
            Some(Command::SymbolFunction("MSFT US Equity".into(), Function::SessionBrief))
        );
        assert!(!matches!(
            parse_brief("BRIEF 5"),
            Some(Command::Function(Function::SessionBrief) | Command::SymbolFunction(_, Function::SessionBrief))
        ));
        assert!(crate::command::hint("BRIEF").contains("Session brief"));

        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut pane = SessionBriefPane::new("NVDA");
        pane.request(4, &worker);
        match requests.try_recv().unwrap() {
            Request::SessionBrief { pane: id, symbol, generation } => {
                assert_eq!((id, generation, symbol.as_str()), (4, 1, "NVDA US Equity"));
            }
            _ => panic!("unexpected request"),
        }
    }

    fn parse_brief(input: &str) -> Option<Command> {
        crate::command::parse(input)
    }

    #[test]
    fn session_brief_pane_rejects_sample_reads() {
        let mut brief = tr_data::session_brief::SessionBrief {
            symbol: "AAPL US Equity".into(),
            ticker: "AAPL".into(),
            timeframe: tr_core::Timeframe::M5,
            date: None,
            provenance: Provenance::Sample,
            fresh: false,
            lines: Vec::new(),
        };
        brief.provenance = Provenance::Sample;
        let mut pane = SessionBriefPane::new("AAPL");
        pane.generation = 1;
        pane.accept(1, Ok(brief));
        assert!(pane.brief.is_none());
        assert!(pane.error.as_deref().unwrap().contains("SAMPLE"));
    }
}
