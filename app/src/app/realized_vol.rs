//! Realized volatility (`RVAR`) of the latest cash session.
use super::{PaneKind, TerminalApp};
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::{congress_view::Action, dock::PaneId, worker::Request};
use egui::{RichText, Ui};
use tr_data::cash_session::{letter_bar_token, window_count_token, without_letter_bar_tokens};
use tr_data::realized_vol::{self, RealizedVol};

pub struct RealizedVolPane {
    pub symbol: String,
    pub spec: String,
    pub tf: String,
    pub generation: u64,
    pub vol: Option<RealizedVol>,
    pub loading: bool,
    pub error: Option<String>,
}

impl RealizedVolPane {
    pub fn new(symbol: impl Into<String>, spec: impl Into<String>) -> Self {
        let symbol = symbol.into();
        let symbol = if symbol.trim().is_empty() { symbol } else { tr_data::security(&symbol).key() };
        Self { symbol, spec: spec.into(), tf: "5m".into(), generation: 0, vol: None, loading: false, error: None }
    }

    pub fn request(&mut self, pane: PaneId, worker: &crate::worker::Worker) {
        self.generation += 1;
        self.vol = None;
        let symbol = self.symbol.trim();
        if symbol.is_empty() {
            self.error = Some("RVAR needs a ticker (AAPL RVAR). Sample prices are never used.".into());
            self.loading = false;
            return;
        }
        if let Err(err) = realized_vol::parse_query(&self.spec, &self.tf) {
            self.error = Some(err);
            self.loading = false;
            return;
        }
        self.loading = true;
        self.error = None;
        self.vol = None;
        worker.send(Request::RealizedVol {
            pane,
            generation: self.generation,
            symbol: tr_data::security(symbol).key(),
            spec: self.spec.clone(),
            timeframe: self.tf.clone(),
        });
    }

    pub fn accept(&mut self, generation: u64, result: Result<RealizedVol, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(vol) if !vol.provenance.is_verified() => {
                self.vol = None;
                self.error = Some(format!(
                    "Realized volatility refused: {}. Verified real data is required.",
                    vol.provenance.label()
                ));
            }
            Ok(vol) => {
                self.tf = vol.timeframe.label().into();
                self.vol = Some(vol);
                self.error = None;
            }
            Err(err) => {
                self.vol = None;
                self.error = Some(err);
            }
        }
    }
}

impl TerminalApp {
    pub(super) fn open_realized_vol(&mut self, symbol: Option<&str>, spec: &str) {
        let symbol = symbol.map(|s| tr_data::security(s).key()).or_else(|| self.focused_symbol()).unwrap_or_default();
        let mut pane = RealizedVolPane::new(symbol, spec);
        if pane.symbol.is_empty() {
            pane.error = Some("RVAR needs a ticker (AAPL RVAR). Sample prices are never used.".into());
        }
        let ticker = if pane.symbol.is_empty() { "Vol".into() } else { tr_data::security(&pane.symbol).ticker };
        self.status = format!("{ticker}: realized volatility of the latest cash session");
        self.open_panel(PaneKind::RealizedVol(Box::new(pane)));
    }

    pub(super) fn realized_vol_pane(&mut self, ui: &mut Ui, id: PaneId) -> Vec<Action> {
        let i = self.idx(id).expect("realized volatility pane");
        let sample_offline = self.provider.eq_ignore_ascii_case("sample");
        let mut reload = false;
        {
            let PaneKind::RealizedVol(pane) = &mut self.panes[i].kind else { return vec![] };
            if !pane.loading && pane.vol.is_none() && pane.error.is_none() && !pane.symbol.trim().is_empty() {
                pane.request(id, &self.worker);
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Realized volatility").strong().color(text()));
                ui.label(RichText::new("observed log-return volatility").font(mono(11.0)).color(amber()));
                if ui.small_button("Reload").on_hover_text("Read verified regular-session bars again").clicked() {
                    reload = true;
                }
            });
            ui.label(RichText::new("Latest cash session only. RV = 100 × sqrt(sum(log(close / previous close)²)). Rolling RV needs the selected number of consecutive returns. No annualization or forecast. Gaps break the rolling window and blank cumulative RV thereafter.").font(ui_font(11.5)).color(muted()));
            if sample_offline {
                ui.label(
                    RichText::new(
                        "Sample / offline display mode does not feed this pane. This pane never falls back to sample bars.",
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
                ui.label(RichText::new("Returns").color(muted()));
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
            if let Some(err) = &pane.error {
                ui.label(RichText::new(err).font(mono(12.0)).color(theme::down()));
            } else if pane.loading {
                ui.label(RichText::new("Reading verified intraday bars…").font(mono(12.0)).color(muted()));
            }
            if let Some(vol) = &pane.vol {
                render_vol(ui, vol);
            }
        }
        if reload && let PaneKind::RealizedVol(pane) = &mut self.panes[i].kind {
            pane.request(id, &self.worker);
        }
        Vec::new()
    }
}

fn render_vol(ui: &mut Ui, vol: &RealizedVol) {
    ui.label(
        RichText::new(format!(
            "{} · {} · {} · {}",
            vol.date,
            vol.timeframe.label(),
            vol.provenance.label(),
            vol.source
        ))
        .font(mono(11.5))
        .color(muted()),
    );
    ui.label(
        RichText::new(format!("{} returns per rolling window · {} unusable/missing buckets", vol.window, vol.gaps))
            .color(amber()),
    );
    let fmt = |v: Option<f64>| v.map(|x| format!("{x:.4}%")).unwrap_or_else(|| "—".into());
    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("realized_vol_grid").striped(true).show(ui, |ui| {
            ui.label("Time (NY)");
            ui.label("Session RV");
            ui.label("Rolling RV");
            ui.end_row();
            for point in vol.points.iter().rev() {
                ui.label(realized_vol::clock_label(point.ts));
                ui.label(fmt(point.cumulative));
                ui.label(fmt(point.rolling));
                ui.end_row();
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Command, Function};
    #[test]
    fn command_request_and_layout_round_trip() {
        assert_eq!(
            crate::command::parse("AAPL RVAR 10 15m"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::RealizedVol { spec: "10 15m".into() }))
        );
        let mut app = crate::app::tests::test_app();
        app.apply(Function::RealizedVol { spec: "10 15m".into() });
        let p = app.panes.iter().find(|p| matches!(p.kind, PaneKind::RealizedVol(_))).unwrap();
        let restored = PaneKind::from_settings(p.kind.settings());
        assert!(matches!(restored, PaneKind::RealizedVol(_)));
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        let mut p = RealizedVolPane::new("AAPL", "10 15m");
        p.request(7, &worker);
        assert!(matches!(requests.try_recv().unwrap(), Request::RealizedVol { pane: 7, generation: 1, .. }));
        p.accept(0, Err("stale".into()));
        assert!(p.loading);
        assert!(p.error.is_none());
        p.accept(1, Err("failed".into()));
        assert!(!p.loading);
        assert_eq!(p.error.as_deref(), Some("failed"));
        p.spec = "bad".into();
        p.request(7, &worker);
        p.accept(1, Err("late".into()));
        assert_ne!(p.error.as_deref(), Some("late"));
    }
}
