//! Combined ALERTS pane: price/setup rules plus the existing congressional list.

use egui::{RichText, ScrollArea, Sense, Ui};
use tr_data::price_alerts::{AlertKind, PriceSide, SetupChange};
use tr_data::whats_news::SETUPS;

use super::TerminalApp;
use crate::congress_view::{self, Action};
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::worker::Request;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertDraftMode {
    Price,
    Vwap,
    Setup,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertDraft {
    pub symbol: String,
    pub mode: AlertDraftMode,
    pub side: PriceSide,
    pub level: String,
    pub setup: String,
    pub change: SetupChange,
}

impl Default for AlertDraft {
    fn default() -> Self {
        Self {
            symbol: String::new(),
            mode: AlertDraftMode::Price,
            side: PriceSide::Above,
            level: String::new(),
            setup: "NEAR_HIGH".into(),
            change: SetupChange::Enter,
        }
    }
}

impl TerminalApp {
    pub(super) fn alerts_pane(&mut self, ui: &mut Ui) -> (Vec<Action>, bool) {
        let mut out = Vec::new();
        let mut mark_congress = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Price and setup alerts").font(ui_font(12.5)).color(text()));
            if ui.small_button("Refresh").clicked() {
                self.worker.send(Request::EvaluatePriceAlerts);
                self.worker.send(Request::Alerts);
            }
            let unread = self.unread_alerts();
            ui.label(RichText::new(format!("{unread} unread")).font(mono(12.0)).color(if unread > 0 {
                amber()
            } else {
                muted()
            }));
            if ui.button(RichText::new("MARK READ").font(mono(11.5))).clicked() {
                self.worker.send(Request::MarkPriceAlertsSeen);
                mark_congress = true;
            }
        });
        ui.add_space(2.0);
        banners(ui, &self.price_alerts, self.provider.eq_ignore_ascii_case("sample"));
        self.alert_form(ui);
        ui.add_space(4.0);
        ui.label(RichText::new("RULES").font(mono(11.0)).color(muted()));
        self.alert_rules(ui);
        ui.add_space(4.0);
        ui.label(RichText::new("FIRED").font(mono(11.0)).color(muted()));
        ui.label(RichText::new("TIME              NAME     MESSAGE").font(mono(11.0)).color(muted()));
        self.alert_events(ui, &mut out);
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(RichText::new("Congressional filings").font(ui_font(12.5)).color(text()));
        let (a, seen) = congress_view::alerts(ui, &self.alerts);
        out.extend(a);
        (out, mark_congress || seen)
    }

    fn alert_form(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Add").font(mono(11.5)).color(muted()));
            if self.alert_draft.symbol.is_empty()
                && let Some(s) = self.focused_symbol()
            {
                self.alert_draft.symbol = tr_data::security(&s).ticker;
            }
            ui.add(egui::TextEdit::singleline(&mut self.alert_draft.symbol).desired_width(72.0).hint_text("ticker"));
            if ui.selectable_label(self.alert_draft.mode == AlertDraftMode::Price, "Price").clicked() {
                self.alert_draft.mode = AlertDraftMode::Price;
            }
            if ui.selectable_label(self.alert_draft.mode == AlertDraftMode::Vwap, "VWAP").clicked() {
                self.alert_draft.mode = AlertDraftMode::Vwap;
            }
            if ui.selectable_label(self.alert_draft.mode == AlertDraftMode::Setup, "Setup").clicked() {
                self.alert_draft.mode = AlertDraftMode::Setup;
            }
            match self.alert_draft.mode {
                AlertDraftMode::Price => {
                    if ui.selectable_label(self.alert_draft.side == PriceSide::Above, "above").clicked() {
                        self.alert_draft.side = PriceSide::Above;
                    }
                    if ui.selectable_label(self.alert_draft.side == PriceSide::Below, "below").clicked() {
                        self.alert_draft.side = PriceSide::Below;
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.alert_draft.level).desired_width(72.0).hint_text("level"),
                    );
                }
                AlertDraftMode::Vwap => {
                    if ui.selectable_label(self.alert_draft.side == PriceSide::Above, "above").clicked() {
                        self.alert_draft.side = PriceSide::Above;
                    }
                    if ui.selectable_label(self.alert_draft.side == PriceSide::Below, "below").clicked() {
                        self.alert_draft.side = PriceSide::Below;
                    }
                }
                AlertDraftMode::Setup => {
                    egui::ComboBox::from_id_salt("alert-setup").selected_text(&self.alert_draft.setup).show_ui(
                        ui,
                        |ui| {
                            for name in SETUPS {
                                ui.selectable_value(&mut self.alert_draft.setup, (*name).to_string(), *name);
                            }
                        },
                    );
                    for (c, label) in
                        [(SetupChange::Enter, "enter"), (SetupChange::Exit, "exit"), (SetupChange::Either, "change")]
                    {
                        if ui.selectable_label(self.alert_draft.change == c, label).clicked() {
                            self.alert_draft.change = c;
                        }
                    }
                }
            }
            if ui.button(RichText::new("WATCH").font(mono(11.5))).clicked() {
                match self.draft_kind() {
                    Ok(kind) => {
                        let symbol = self.alert_draft.symbol.clone();
                        self.set_price_alert(Some(symbol), kind);
                    }
                    Err(e) => self.status = e,
                }
            }
        });
        ui.label(
            RichText::new(
                "AAPL ALERT 150 · AAPL ALERT VWAP · AAPL ALERT VWAP BELOW · NVDA ALERT SETUP NEAR_HIGH · click a fired row for the chart",
            )
            .font(ui_font(11.0))
            .color(muted()),
        );
    }

    fn draft_kind(&self) -> Result<AlertKind, String> {
        match self.alert_draft.mode {
            AlertDraftMode::Price => {
                let level: f64 = self.alert_draft.level.trim().parse().map_err(|_| "Type a price level".to_string())?;
                if !level.is_finite() || level <= 0.0 {
                    return Err("Price level must be a positive number".into());
                }
                Ok(AlertKind::Price { side: self.alert_draft.side, level })
            }
            AlertDraftMode::Vwap => Ok(AlertKind::Vwap { side: self.alert_draft.side }),
            AlertDraftMode::Setup => {
                let setup = tr_data::price_alerts::normalize_setup(&self.alert_draft.setup)
                    .ok_or_else(|| "Pick a SCAN setup".to_string())?;
                Ok(AlertKind::Setup { setup, change: self.alert_draft.change })
            }
        }
    }

    fn alert_rules(&mut self, ui: &mut Ui) {
        if self.price_alerts.rules.is_empty() {
            ui.label(RichText::new("No price, VWAP, or setup rules yet.").font(mono(12.0)).color(muted()));
            return;
        }
        let rules = self.price_alerts.rules.clone();
        for r in rules {
            ui.horizontal(|ui| {
                let ticker = tr_data::security(&r.symbol).ticker;
                let resp = ui.add(
                    egui::Label::new(RichText::new(format!("{ticker:<8}")).font(mono(12.0)).color(text()))
                        .sense(Sense::click()),
                );
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    self.open_security(&r.symbol);
                }
                ui.label(RichText::new(r.kind.label()).font(mono(12.0)).color(text()));
                let mut on = r.enabled;
                if ui.checkbox(&mut on, "on").changed() {
                    self.worker.send(Request::SetPriceAlertEnabled { id: r.id.clone(), enabled: on });
                }
                if ui.small_button("Remove").clicked() {
                    self.worker.send(Request::RemovePriceAlert { query: r.id });
                }
            });
        }
    }

    fn alert_events(&mut self, ui: &mut Ui, out: &mut Vec<Action>) {
        if self.price_alerts.events.is_empty() {
            ui.label(
                RichText::new("No price, VWAP, or setup alerts have fired. Sample bars never invent them.")
                    .font(mono(12.0))
                    .color(muted()),
            );
            return;
        }
        ScrollArea::vertical().id_salt("price-alerts").max_height(180.0).auto_shrink([false, false]).show(ui, |ui| {
            for a in &self.price_alerts.events {
                ui.horizontal(|ui| {
                    let ts = chrono::DateTime::from_timestamp(a.created_at, 0)
                        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_default();
                    ui.label(RichText::new(ts).font(mono(12.0)).color(if a.seen { muted() } else { amber() }));
                    let ticker = tr_data::security(&a.symbol).ticker;
                    let name = ui.add(
                        egui::Label::new(RichText::new(format!("{ticker:<8}")).font(mono(12.0)).color(text()))
                            .sense(Sense::click()),
                    );
                    if name.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if name.clicked() {
                        out.push(Action::OpenChart(a.symbol.clone()));
                    }
                    let msg = ui.add(
                        egui::Label::new(RichText::new(&a.message).font(mono(12.0)).color(if a.seen {
                            muted()
                        } else {
                            text()
                        }))
                        .sense(Sense::click()),
                    );
                    if msg.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if msg.clicked() {
                        out.push(Action::OpenChart(a.symbol.clone()));
                    }
                });
            }
        });
    }
}

fn banners(ui: &mut Ui, batch: &tr_data::price_alerts::AlertBatch, offline: bool) {
    use tr_data::news::format_freshness;
    let now = chrono::Utc::now().timestamp();
    if batch.fetched_at > 0 {
        let mut meta = vec![format_freshness(batch.fetched_at, now)];
        if !batch.queried.is_empty() {
            meta.push(format!("{} name{}", batch.queried.len(), if batch.queried.len() == 1 { "" } else { "s" }));
        }
        meta.push(format!("{} rule{}", batch.rules.len(), if batch.rules.len() == 1 { "" } else { "s" }));
        ui.label(RichText::new(meta.join(" · ")).font(ui_font(11.0)).color(muted()));
    }
    if batch.sample || offline {
        ui.label(
            RichText::new(
                "Sample / offline mode: price, VWAP, and setup alerts are not invented and never enter research.",
            )
            .font(ui_font(12.0))
            .color(theme::down()),
        );
    }
    if batch.is_stale_at(now) {
        ui.label(
            RichText::new(format!(
                "Stale feed — last successful check {}. Crossings are not fired from stale quotes or bars.",
                format_freshness(batch.fetched_at, now)
            ))
            .font(ui_font(12.0))
            .color(amber()),
        );
    }
    if batch.is_partial() {
        let failed = batch
            .failures
            .iter()
            .map(|f| format!("{} ({})", tr_data::security(&f.symbol).ticker, f.error))
            .collect::<Vec<_>>()
            .join(" · ");
        ui.label(
            RichText::new(format!(
                "Partial: {} of {} names failed. {failed}",
                batch.failures.len(),
                batch.queried.len().max(batch.failures.len())
            ))
            .font(ui_font(12.0))
            .color(amber()),
        );
    } else if batch.events.is_empty() && !batch.failures.is_empty() && batch.rules.iter().any(|r| r.enabled) {
        ui.label(
            RichText::new(format!("Could not evaluate alerts: {}", batch.failures[0].error))
                .font(ui_font(12.0))
                .color(theme::down()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::TerminalApp;
    use super::*;
    use crate::app::tests::test_app;
    use egui::{Pos2, Rect, pos2, vec2};
    use tr_data::price_alerts::{AlertKind, PriceSide};

    fn paint(ctx: &egui::Context, app: &mut TerminalApp, events: Vec<egui::Event>) -> Vec<egui::epaint::ClippedShape> {
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(ctx.input(|i| i.time) + 0.2),
                events,
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1100.0, 240.0))),
                ..Default::default()
            },
            |ui| app.alert_form(ui),
        );
        output.textures_delta.clear();
        output.shapes
    }

    fn text_position(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Pos2 {
        fn find(shape: &egui::Shape, needle: &str) -> Option<Pos2> {
            match shape {
                egui::Shape::Text(t) if t.galley.job.text == needle => Some(t.pos + t.galley.size() * 0.5),
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, needle)),
                _ => None,
            }
        }
        shapes.iter().find_map(|s| find(&s.shape, needle)).unwrap_or_else(|| panic!("missing UI text {needle:?}"))
    }

    fn galley_contains(shapes: &[egui::epaint::ClippedShape], needle: &str) -> bool {
        fn find(shape: &egui::Shape, needle: &str) -> bool {
            match shape {
                egui::Shape::Text(t) if t.galley.job.text.contains(needle) => true,
                egui::Shape::Vec(v) => v.iter().any(|s| find(s, needle)),
                _ => false,
            }
        }
        shapes.iter().any(|s| find(&s.shape, needle))
    }

    fn pointer(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn click(ctx: &egui::Context, app: &mut TerminalApp, shapes: &[egui::epaint::ClippedShape], label: &str) {
        let pos = text_position(shapes, label);
        paint(ctx, app, vec![egui::Event::PointerMoved(pos), pointer(pos, true)]);
        paint(ctx, app, vec![pointer(pos, false)]);
    }

    #[test]
    fn vwap_form_watches_a_cross_without_a_price_level() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.alert_draft.symbol = "AAPL".into();
        let shapes = paint(&ctx, &mut app, vec![]);
        assert!(galley_contains(&shapes, "AAPL ALERT VWAP"));
        click(&ctx, &mut app, &shapes, "VWAP");
        assert_eq!(app.alert_draft.mode, AlertDraftMode::Vwap);
        let shapes = paint(&ctx, &mut app, vec![]);
        click(&ctx, &mut app, &shapes, "below");
        assert_eq!(app.draft_kind().unwrap(), AlertKind::Vwap { side: PriceSide::Below });
        let shapes = paint(&ctx, &mut app, vec![]);
        click(&ctx, &mut app, &shapes, "WATCH");
        assert!(
            app.panes.iter().any(|p| matches!(p.kind, super::super::PaneKind::Alerts)),
            "watching a VWAP cross opens the alerts pane"
        );
        assert!(!app.status.contains("price level"));
    }
}
