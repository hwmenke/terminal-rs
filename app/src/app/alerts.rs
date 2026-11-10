//! Combined ALERTS pane: price/setup rules plus the existing congressional list.

use egui::{RichText, ScrollArea, Sense, Ui};
use tr_data::price_alerts::{AlertKind, PriceSide, SetupChange};
use tr_data::session_alerts::SessionSignal;
use tr_data::whats_news::SETUPS;

use super::TerminalApp;
use crate::congress_view::{self, Action};
use crate::theme::{self, amber, mono, muted, text, ui_font};
use crate::worker::Request;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertDraftMode {
    Combo,
    Price,
    Vwap,
    Rvol,
    Setup,
    Session,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertDraft {
    pub symbol: String,
    pub mode: AlertDraftMode,
    pub side: PriceSide,
    pub level: String,
    pub multiple: String,
    pub setup: String,
    pub change: SetupChange,
    pub signal: SessionSignal,
}

impl Default for AlertDraft {
    fn default() -> Self {
        Self {
            symbol: String::new(),
            mode: AlertDraftMode::Price,
            side: PriceSide::Above,
            level: String::new(),
            multiple: String::new(),
            setup: "NEAR_HIGH".into(),
            change: SetupChange::Enter,
            signal: SessionSignal::Path,
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
                self.worker.send(Request::MarkTradePlansSeen);
                mark_congress = true;
            }
        });
        ui.add_space(2.0);
        banners(ui, &self.price_alerts, self.provider.eq_ignore_ascii_case("sample"));
        self.alert_form(ui);
        self.trade_plans_section(ui, &mut out);
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
            if ui.selectable_label(self.alert_draft.mode == AlertDraftMode::Rvol, "RVOL").clicked() {
                self.alert_draft.mode = AlertDraftMode::Rvol;
            }
            if ui.selectable_label(self.alert_draft.mode == AlertDraftMode::Setup, "Setup").clicked() {
                self.alert_draft.mode = AlertDraftMode::Setup;
            }
            if ui.selectable_label(self.alert_draft.mode == AlertDraftMode::Session, "Session").clicked() {
                self.alert_draft.mode = AlertDraftMode::Session;
            }
            if ui.selectable_label(self.alert_draft.mode == AlertDraftMode::Combo, "Setup + price + RVOL").clicked() {
                self.alert_draft.mode = AlertDraftMode::Combo;
            }
            match self.alert_draft.mode {
                AlertDraftMode::Combo => {
                    egui::ComboBox::from_id_salt("combo-setup").selected_text(&self.alert_draft.setup).show_ui(
                        ui,
                        |ui| {
                            for name in SETUPS.iter().filter(|s| **s != "ALL" && **s != "ONEIL_LENS") {
                                ui.selectable_value(&mut self.alert_draft.setup, (*name).to_string(), *name);
                            }
                        },
                    );
                    ui.selectable_value(&mut self.alert_draft.side, PriceSide::Above, "above");
                    ui.selectable_value(&mut self.alert_draft.side, PriceSide::Below, "below");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.alert_draft.level).desired_width(72.0).hint_text("level"),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.alert_draft.multiple)
                            .desired_width(72.0)
                            .hint_text("RVOL min"),
                    );
                }
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
                AlertDraftMode::Rvol => {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.alert_draft.level)
                            .desired_width(72.0)
                            .hint_text("multiple"),
                    );
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
                AlertDraftMode::Session => {
                    for (signal, label) in [
                        (SessionSignal::Path, "Path"),
                        (SessionSignal::Pace, "Pace"),
                        (SessionSignal::Gap, "Gap"),
                        (SessionSignal::Extreme, "Extreme"),
                        (SessionSignal::Relative, "vs SPY"),
                    ] {
                        if ui.selectable_label(self.alert_draft.signal == signal, label).clicked() {
                            self.alert_draft.signal = signal;
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
                "AAPL ALERT COMBO NEAR_HIGH ABOVE 250 RVOL 2 · previous session setup; M5 crossing with RVOL at that print · AAPL ALERT 150 · AAPL ALERT VWAP · AAPL ALERT RVOL 2 · AAPL ALERT PATH · AAPL ALERT PACE · AAPL ALERT GAP · AAPL ALERT XTRM · AAPL ALERT RPATH · NVDA ALERT SETUP NEAR_HIGH · click a fired row for the chart",
            )
            .font(ui_font(11.0))
            .color(muted()),
        );
    }

    fn draft_kind(&self) -> Result<AlertKind, String> {
        match self.alert_draft.mode {
            AlertDraftMode::Combo => {
                let setup = tr_data::price_alerts::normalize_combo_setup(&self.alert_draft.setup)
                    .ok_or("Pick an individual setup")?;
                let level: f64 = self.alert_draft.level.trim().parse().map_err(|_| "Type a price level")?;
                let multiple: f64 = self.alert_draft.multiple.trim().parse().map_err(|_| "Type an RVOL multiple")?;
                if !level.is_finite() || level <= 0.0 || !multiple.is_finite() || multiple <= 0.0 {
                    return Err("Price and RVOL must be positive numbers".into());
                }
                Ok(AlertKind::SetupPriceRvol { setup, side: self.alert_draft.side, level, multiple })
            }
            AlertDraftMode::Price => {
                let level: f64 = self.alert_draft.level.trim().parse().map_err(|_| "Type a price level".to_string())?;
                if !level.is_finite() || level <= 0.0 {
                    return Err("Price level must be a positive number".into());
                }
                Ok(AlertKind::Price { side: self.alert_draft.side, level })
            }
            AlertDraftMode::Vwap => Ok(AlertKind::Vwap { side: self.alert_draft.side }),
            AlertDraftMode::Rvol => {
                let multiple: f64 =
                    self.alert_draft.level.trim().parse().map_err(|_| "Type an RVOL multiple".to_string())?;
                if !multiple.is_finite() || multiple <= 0.0 {
                    return Err("RVOL multiple must be a positive number".into());
                }
                Ok(AlertKind::Rvol { multiple })
            }
            AlertDraftMode::Setup => {
                let setup = tr_data::price_alerts::normalize_setup(&self.alert_draft.setup)
                    .ok_or_else(|| "Pick a SCAN setup".to_string())?;
                Ok(AlertKind::Setup { setup, change: self.alert_draft.change })
            }
            AlertDraftMode::Session => Ok(AlertKind::Session { signal: self.alert_draft.signal }),
        }
    }

    fn trade_plans_section(&mut self, ui: &mut Ui, out: &mut Vec<Action>) {
        use tr_data::trade_plans::{PlanChange, PlanStatus, locked_r, open_r, planned_r, stop_label};
        ui.add_space(6.0);
        ui.label(RichText::new("TRADE PLANS").font(mono(11.0)).color(muted()));
        if self.trade_plans.sample || self.provider.eq_ignore_ascii_case("sample") {
            ui.label(
                RichText::new("Sample / offline mode: trade plans are not evaluated and never fire.")
                    .color(theme::down()),
            );
        }
        let now = chrono::Utc::now().timestamp();
        if self.trade_plans.stale || tr_data::price_alerts::is_stale(self.trade_plans.fetched_at, now) {
            ui.label(RichText::new("Stale quotes — plans hold their last verified state.").color(amber()));
        }
        if !self.trade_plans.failures.is_empty() {
            let failed = self
                .trade_plans
                .failures
                .iter()
                .map(|f| format!("{} ({})", tr_data::security(&f.symbol).ticker, f.error))
                .collect::<Vec<_>>()
                .join(" · ");
            ui.label(RichText::new(format!("Partial: {failed}")).color(amber()));
        }
        let mut plans: Vec<_> = self.trade_plans.plans.iter().collect();
        plans.sort_by_key(|p| (!p.status.active(), std::cmp::Reverse(p.closed_at.unwrap_or(p.created_at))));
        let mut closed = 0;
        if plans.is_empty() {
            ui.label(
                RichText::new("No trade plans. Arm one from DESK or NVDA PLAN 142.5 STOP 138.2 TARGET 151.1 RISK 300.")
                    .font(mono(12.0))
                    .color(muted()),
            );
        }
        ScrollArea::vertical().id_salt("trade-plan-rows").max_height(260.0).show(ui, |ui| {
            for plan in plans {
                if !plan.status.active() {
                    closed += 1;
                    if closed > 10 {
                        continue;
                    }
                }
                ui.push_id(&plan.id, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let ticker = tr_data::security(&plan.symbol).ticker;
                        if ui
                            .add(
                                egui::Label::new(RichText::new(ticker).font(mono(12.0)).color(text()))
                                    .sense(Sense::click()),
                            )
                            .clicked()
                        {
                            out.push(Action::OpenChart(plan.symbol.clone()));
                        }
                        ui.label(RichText::new(if plan.long { "Long" } else { "Short" }).font(mono(12.0)));
                        let color = match plan.status {
                            PlanStatus::Armed | PlanStatus::Triggered => amber(),
                            PlanStatus::Stopped | PlanStatus::Invalidated => theme::down(),
                            PlanStatus::TargetHit => theme::up(),
                            PlanStatus::Cancelled => muted(),
                        };
                        ui.label(RichText::new(plan.status.label()).font(mono(12.0)).color(color));
                        let direction = match plan.entry_side {
                            Some(PriceSide::Above) => " ↑",
                            Some(PriceSide::Below) => " ↓",
                            None => "",
                        };
                        let target = plan
                            .target
                            .map(|p| format!("  target {p:.2} ({:.1}R)", planned_r(plan, p)))
                            .unwrap_or_default();
                        let size = if plan.dollar_risk == 0.0 {
                            "size not set".into()
                        } else {
                            format!("{:.0} sh  ${:.0} risk", plan.shares, plan.dollar_risk)
                        };
                        let moved = locked_r(plan).map(|r| format!(" (initial {:.2}) · locks {r:+.2}R", plan.initial_stop())).unwrap_or_default();
                        ui.label(
                            RichText::new(format!(
                                "entry {:.2}{direction}  {} {:.2}{moved}{target}  {size}",
                                plan.entry, stop_label(plan), plan.stop
                            )).font(mono(12.0)),
                        ).on_hover_text("at observed quote, not a fill; no order was changed\ntrail measured from observed quotes; moves while the app was closed are not backfilled");
                        if let Some(r) = plan.breakeven_at_r {
                            ui.label(RichText::new(format!("BE@{r}R")).font(mono(11.0)).color(muted()));
                        }
                        if let Some(trail) = &plan.trail {
                            ui.label(RichText::new(format!("trail {trail}")).font(mono(11.0)).color(muted())).on_hover_text(&trail.source);
                        }
                        if plan.status == PlanStatus::Triggered
                            && plan.last_price.is_some_and(|p| if plan.long { p > plan.entry } else { p < plan.entry })
                            && (if plan.long { plan.stop < plan.entry } else { plan.stop > plan.entry })
                            && ui.small_button("Stop→BE").on_hover_text("Move the plan stop to entry using the observed quote; no order was changed").clicked()
                        {
                            self.worker.send(Request::UpdateTradePlan { query: plan.id.clone(), change: PlanChange::MoveStop(plan.entry) });
                        }
                        if plan.status.active() {
                            if ui.small_button("Cancel").clicked() {
                                self.worker.send(Request::CancelTradePlan { id: plan.id.clone() });
                            }
                        } else {
                            if ui.small_button("Journal").clicked() {
                                out.push(Action::Command(format!("{} JOURNAL", plan.symbol)));
                            }
                            if ui.small_button("Remove").clicked() {
                                self.worker.send(Request::RemoveTradePlan { query: plan.id.clone() });
                            }
                        }
                    });
                    let as_of = plan
                        .last_tick_ts
                        .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                        .map(|ts| ts.format("%H:%M UTC").to_string())
                        .unwrap_or_default();
                    let observation = if let Some(r) = open_r(plan) {
                        format!("open R {r:+.2} as of {as_of}")
                    } else if plan.last_price.is_none() {
                        "awaiting first verified quote".into()
                    } else if plan.status == PlanStatus::Armed {
                        format!("awaiting cross · as of {as_of}")
                    } else {
                        format!("as of {as_of}")
                    };
                    ui.label(RichText::new(format!("{observation} — at observed quote, not a fill; no order was changed")).font(mono(11.0)).color(muted()));
                });
            }
        });
        ScrollArea::vertical().id_salt("trade-plan-events").max_height(180.0).show(ui, |ui| {
            for event in &self.trade_plans.events {
                ui.horizontal_wrapped(|ui| {
                    let ts = chrono::DateTime::from_timestamp(event.created_at, 0)
                        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_default();
                    ui.label(RichText::new(ts).font(mono(12.0)).color(if event.seen { muted() } else { amber() }));
                    for label in [tr_data::security(&event.symbol).ticker, event.message.clone()] {
                        let response = ui.add(
                            egui::Label::new(RichText::new(label).font(mono(12.0)).color(if event.seen {
                                muted()
                            } else {
                                text()
                            }))
                            .sense(Sense::click()),
                        );
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if response.clicked() {
                            out.push(Action::OpenChart(event.symbol.clone()));
                        }
                    }
                });
            }
        });
        ui.label(
            RichText::new("Plans watch quotes only. They place no orders, and R is at the observed quote, not a fill.")
                .font(ui_font(11.0))
                .color(muted()),
        );
    }

    fn alert_rules(&mut self, ui: &mut Ui) {
        if self.price_alerts.rules.is_empty() {
            ui.label(
                RichText::new("No price, VWAP, RVOL, session, or setup rules yet.").font(mono(12.0)).color(muted()),
            );
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
                RichText::new(
                    "No price, VWAP, RVOL, session, or setup alerts have fired. Sample bars never invent them.",
                )
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
                "Sample / offline mode: price, VWAP, RVOL, and setup alerts are not invented and never enter research.",
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
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1100.0, 700.0))),
                ..Default::default()
            },
            |ui| {
                app.alert_form(ui);
                app.trade_plans_section(ui, &mut Vec::new());
                app.alert_rules(ui);
            },
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

    fn plan_store() -> tr_data::trade_plans::Store {
        let mut store = tr_data::trade_plans::Store::default();
        store
            .arm(
                tr_data::trade_plans::PlanDraft {
                    symbol: "NVDA".into(),
                    entry: 100.0,
                    stop: 90.0,
                    target: Some(120.0),
                    dollar_risk: 300.0,
                    shares: 30.0,
                    stop_source: "command".into(),
                    breakeven_at_r: None,
                    trail: None,
                },
                100,
            )
            .unwrap();
        store
    }

    #[test]
    fn trade_plan_row_shows_trail_initial_stop_and_locked_r() {
        use tr_data::trade_plans::{PlanStatus, TrailKind, TrailSpec};
        let ctx = egui::Context::default();
        let mut app = test_app();
        let mut store = plan_store();
        let plan = &mut store.plans[0];
        plan.status = PlanStatus::Triggered;
        plan.breakeven_at_r = Some(1.0);
        plan.trail = Some(TrailSpec { kind: TrailKind::R, value: 2.0, source: "command".into() });
        plan.stop = 105.0;
        plan.last_price = Some(115.0);
        plan.last_tick_ts = Some(chrono::Utc::now().timestamp());
        app.trade_plans = store.snapshot(chrono::Utc::now().timestamp(), false, false, vec![]);
        let shapes = paint(&ctx, &mut app, vec![]);
        for label in [
            "trailing stop 105.00",
            "initial 90.00",
            "locks +0.50R",
            "BE@1R",
            "trail 2R",
            "open R +1.50",
            "target 120.00 (2.0R)",
            "at observed quote, not a fill",
            "no order was changed",
        ] {
            assert!(galley_contains(&shapes, label), "missing {label}");
        }
    }

    #[test]
    fn stop_to_be_button_sends_move_stop_only_when_in_profit() {
        use tr_data::trade_plans::{PlanChange, PlanStatus};
        for long in [true, false] {
            let ctx = egui::Context::default();
            let mut app = test_app();
            let (worker, mut requests) = crate::worker::Worker::test_channel();
            app.worker = worker;
            let mut store = plan_store();
            let plan = &mut store.plans[0];
            plan.long = long;
            plan.stop = if long { 90.0 } else { 110.0 };
            plan.status = PlanStatus::Triggered;
            plan.last_price = Some(if long { 105.0 } else { 95.0 });
            let id = plan.id.clone();
            app.trade_plans = store.snapshot(chrono::Utc::now().timestamp(), false, false, vec![]);
            let shapes = paint(&ctx, &mut app, vec![]);
            assert!(galley_contains(&shapes, "Stop→BE"));
            click(&ctx, &mut app, &shapes, "Stop→BE");
            assert!(std::iter::from_fn(|| requests.try_recv().ok()).any(
                |r| matches!(r, Request::UpdateTradePlan { query, change: PlanChange::MoveStop(100.0) } if query == id)
            ));
            for mode in 0..5 {
                let plan = &mut app.trade_plans.plans[0];
                plan.status = if mode == 0 { PlanStatus::Armed } else { PlanStatus::Triggered };
                plan.last_price = match mode {
                    1 => None,
                    2 => Some(100.0),
                    3 => Some(if long { 95.0 } else { 105.0 }),
                    _ => Some(if long { 105.0 } else { 95.0 }),
                };
                plan.stop = if mode == 4 {
                    100.0
                } else if long {
                    90.0
                } else {
                    110.0
                };
                let shapes = paint(&ctx, &mut app, vec![]);
                assert!(!galley_contains(&shapes, "Stop→BE"), "mode {mode}");
            }
        }
    }

    #[test]
    fn stop_management_commands_dispatch_only_the_named_plan() {
        use tr_data::trade_plans::{PlanChange, TrailKind};
        let mut app = test_app();
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        app.worker = worker;
        app.execute("NVDA PLAN 100 STOP 90 BE 1 TRAIL 2R");
        assert!(std::iter::from_fn(|| requests.try_recv().ok()).any(|r| matches!(r, Request::ArmTradePlan(d) if d.breakeven_at_r == Some(1.0) && d.trail.as_ref().is_some_and(|t| t.kind == TrailKind::R && t.value == 2.0))));
        for (cmd, expected) in [
            ("NVDA BREAKEVEN 1", PlanChange::Breakeven(Some(1.0))),
            ("MSFT MOVESTOP 145", PlanChange::MoveStop(145.0)),
            ("NVDA TRAIL OFF", PlanChange::Trail(None)),
            ("MSFT BREAKEVEN OFF", PlanChange::Breakeven(None)),
        ] {
            app.execute(cmd);
            let query_expected = tr_data::security(cmd.split_whitespace().next().unwrap()).key();
            assert!(std::iter::from_fn(|| requests.try_recv().ok()).any(|r| matches!(r, Request::UpdateTradePlan { query, change } if query == query_expected && change == expected)), "{cmd}");
        }
    }

    #[test]
    fn plan_commands_dispatch_sized_and_unsized_drafts() {
        let mut app = test_app();
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        app.worker = worker;
        for (command, risk, shares) in [
            ("NVDA PLAN 142.5 STOP 138.2 TARGET 151.1 RISK 300", 300.0, 69.0),
            ("NVDA PLAN 142.5 STOP 138.2 TARGET 151.1", 0.0, 0.0),
        ] {
            app.execute(command);
            let draft = std::iter::from_fn(|| requests.try_recv().ok())
                .find_map(|r| match r {
                    Request::ArmTradePlan(draft) => Some(draft),
                    _ => None,
                })
                .expect("arm command request");
            assert_eq!(draft.symbol, "NVDA US Equity");
            assert_eq!(draft.entry, 142.5);
            assert_eq!(draft.stop, 138.2);
            assert_eq!(draft.target, Some(151.1));
            assert_eq!(draft.dollar_risk, risk);
            assert_eq!(draft.shares, shares);
            assert_eq!(draft.stop_source, "command");
        }
        app.execute("PLANS");
        assert!(std::iter::from_fn(|| requests.try_recv().ok()).any(|r| matches!(r, Request::TradePlans)));
        assert!(app.panes.iter().any(|p| matches!(p.kind, super::super::PaneKind::Alerts)));
        app.execute("UNPLAN NVDA");
        assert!(
            std::iter::from_fn(|| requests.try_recv().ok())
                .any(|r| matches!(r, Request::CancelTradePlansFor { query } if query == "NVDA"))
        );
    }

    #[test]
    fn trade_plan_rows_render_status_and_cancel_sends_request() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        app.worker = worker;
        let store = plan_store();
        let id = store.plans[0].id.clone();
        app.trade_plans = store.snapshot(chrono::Utc::now().timestamp(), false, false, vec![]);
        let shapes = paint(&ctx, &mut app, vec![]);
        for label in [
            "TRADE PLANS",
            "ARMED",
            "entry 100.00",
            "stop 90.00",
            "target 120.00 (2.0R)",
            "30 sh",
            "$300 risk",
            "awaiting first verified quote",
        ] {
            assert!(galley_contains(&shapes, label), "missing {label}");
        }
        click(&ctx, &mut app, &shapes, "Cancel");
        assert!(
            std::iter::from_fn(|| requests.try_recv().ok())
                .any(|r| matches!(r, Request::CancelTradePlan { id: sent } if sent == id))
        );
        app.trade_plans.plans[0].status = tr_data::trade_plans::PlanStatus::Stopped;
        let shapes = paint(&ctx, &mut app, vec![]);
        assert!(galley_contains(&shapes, "STOPPED"));
        assert!(galley_contains(&shapes, "Journal"));
        click(&ctx, &mut app, &shapes, "Remove");
        assert!(
            std::iter::from_fn(|| requests.try_recv().ok())
                .any(|r| matches!(r, Request::RemoveTradePlan { query } if query == id))
        );
    }

    #[test]
    fn plan_event_click_opens_chart_and_counts_unread() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let (worker, _requests, responses) = crate::worker::Worker::test_channel_with_responses();
        app.worker = worker;
        let mut store = plan_store();
        for (price, ts) in [(99.0, 101), (100.0, 102)] {
            let outcome = tr_data::trade_plans::evaluate(
                &mut store,
                tr_data::trade_plans::EvalInput {
                    now: ts,
                    sample_provider: false,
                    prices: vec![tr_data::price_alerts::PriceTick {
                        symbol: "NVDA US Equity".into(),
                        price,
                        ts,
                        provenance: tr_core::Provenance::Real,
                    }],
                    failures: vec![],
                },
            );
            if !outcome.fired.is_empty() {
                responses.send(crate::worker::Response::NewTradePlanEvents(outcome.fired)).unwrap();
            }
        }
        responses.send(crate::worker::Response::TradePlanFeed(store.snapshot(102, false, false, vec![]))).unwrap();
        let previous_unread = app.unread_alerts();
        let previous_toasts = app.toasts.len();
        app.drain();
        assert_eq!(app.unread_alerts(), previous_unread + 1);
        assert_eq!(app.toasts.len(), previous_toasts + 1);
        assert_eq!(app.trade_plans.plans[0].status, tr_data::trade_plans::PlanStatus::Triggered);
        let mut render = |events| {
            let mut actions = Vec::new();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(ctx.input(|i| i.time) + 0.2),
                    events,
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1800.0, 700.0))),
                    ..Default::default()
                },
                |ui| app.trade_plans_section(ui, &mut actions),
            );
            output.textures_delta.clear();
            (output.shapes, actions)
        };
        let (shapes, _) = render(vec![]);
        assert!(galley_contains(&shapes, "open R +0.00 as of 00:01 UTC"));
        assert!(galley_contains(&shapes, "at observed quote, not a fill"));
        let pos = text_position(&shapes, &store.events[0].message);
        render(vec![egui::Event::PointerMoved(pos), pointer(pos, true)]);
        let (_, actions) = render(vec![pointer(pos, false)]);
        assert!(actions.iter().any(|a| matches!(a, Action::OpenChart(s) if s == "NVDA US Equity")));
        store.mark_seen();
        responses.send(crate::worker::Response::TradePlanFeed(store.snapshot(102, false, false, vec![]))).unwrap();
        app.drain();
        assert_eq!(app.unread_alerts(), previous_unread);
    }

    #[test]
    fn combo_feed_and_fired_event_chart_jump() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let (worker, _requests, responses) = crate::worker::Worker::test_channel_with_responses();
        app.worker = worker;
        let kind = AlertKind::SetupPriceRvol {
            setup: "NEAR_HIGH".into(),
            side: PriceSide::Above,
            level: 250.0,
            multiple: 2.0,
        };
        let mut batch = tr_data::price_alerts::AlertBatch::empty(1);
        batch.events.push(tr_data::price_alerts::FiredAlert {
            id: "event".into(),
            rule_id: "combo".into(),
            symbol: "MSFT US Equity".into(),
            message: "Combo fired".into(),
            created_at: 1,
            seen: false,
            kind,
            trigger_key: "event".into(),
        });
        responses.send(crate::worker::Response::PriceAlertFeed(batch.clone())).unwrap();
        app.drain();
        assert_eq!(app.price_alerts, batch);
        assert_eq!(app.price_alerts.unread(), 1);
        let mut render = |events| {
            let mut actions = Vec::new();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(ctx.input(|i| i.time) + 0.2),
                    events,
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1100.0, 700.0))),
                    ..Default::default()
                },
                |ui| app.alert_events(ui, &mut actions),
            );
            output.textures_delta.clear();
            output.textures_delta.clear();
            (output.shapes, actions)
        };
        let (shapes, _) = render(vec![]);
        let pos = text_position(&shapes, "Combo fired");
        render(vec![egui::Event::PointerMoved(pos), pointer(pos, true)]);
        let (_, actions) = render(vec![pointer(pos, false)]);
        assert!(actions.iter().any(|a| matches!(a, Action::OpenChart(symbol) if symbol == "MSFT US Equity")));
        app.open_security("MSFT US Equity");
        assert!(
            app.panes
                .iter()
                .any(|p| matches!(&p.kind, super::super::PaneKind::Chart(c) if c.symbol == "MSFT US Equity"))
        );
    }

    #[test]
    fn combo_form_label_and_worker_request() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        app.worker = worker;
        app.alert_draft.symbol = "aapl".into();
        let shapes = paint(&ctx, &mut app, vec![]);
        click(&ctx, &mut app, &shapes, "Setup + price + RVOL");
        assert_eq!(app.alert_draft.mode, AlertDraftMode::Combo);
        assert!(app.draft_kind().is_err());
        app.alert_draft.level = "250".into();
        app.alert_draft.multiple = "2".into();
        let kind = app.draft_kind().unwrap();
        assert_eq!(kind.label(), "NEAR_HIGH + above 250.00 + RVOL ≥ 2.00×");
        let shapes = paint(&ctx, &mut app, vec![]);
        click(&ctx, &mut app, &shapes, "WATCH");
        assert!(std::iter::from_fn(|| requests.try_recv().ok()).any(
            |r| matches!(r, Request::AddPriceAlert { symbol, kind: k } if symbol == "AAPL US Equity" && k == kind)
        ));
        app.price_alerts.rules.push(tr_data::price_alerts::AlertRule {
            id: "combo".into(),
            symbol: "AAPL US Equity".into(),
            kind: kind.clone(),
            created_at: 1,
            enabled: true,
        });
        let shapes = paint(&ctx, &mut app, vec![]);
        assert!(galley_contains(&shapes, &kind.label()));
        click(&ctx, &mut app, &shapes, "on");
        assert!(
            std::iter::from_fn(|| requests.try_recv().ok())
                .any(|r| matches!(r, Request::SetPriceAlertEnabled { id, enabled: false } if id == "combo"))
        );
        let shapes = paint(&ctx, &mut app, vec![]);
        click(&ctx, &mut app, &shapes, "Remove");
        assert!(
            std::iter::from_fn(|| requests.try_recv().ok())
                .any(|r| matches!(r, Request::RemovePriceAlert { query } if query == "combo"))
        );
        app.alert_draft.setup = "ALL".into();
        assert!(app.draft_kind().is_err());
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

    #[test]
    fn rvol_form_watches_a_multiple_without_inventing_one() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.alert_draft.symbol = "AAPL".into();
        let shapes = paint(&ctx, &mut app, vec![]);
        assert!(galley_contains(&shapes, "AAPL ALERT RVOL 2"));
        click(&ctx, &mut app, &shapes, "RVOL");
        assert_eq!(app.alert_draft.mode, AlertDraftMode::Rvol);
        assert!(app.draft_kind().is_err(), "an empty multiple is not a default of 2");
        app.alert_draft.level = "2".into();
        assert_eq!(app.draft_kind().unwrap(), AlertKind::Rvol { multiple: 2.0 });
        let shapes = paint(&ctx, &mut app, vec![]);
        click(&ctx, &mut app, &shapes, "WATCH");
        assert!(app.panes.iter().any(|p| matches!(p.kind, super::super::PaneKind::Alerts)));
        assert!(!app.status.contains("multiple"));
    }

    #[test]
    fn session_form_watches_pace_without_a_level() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.alert_draft.symbol = "AAPL".into();
        let shapes = paint(&ctx, &mut app, vec![]);
        assert!(galley_contains(&shapes, "AAPL ALERT PATH"));
        click(&ctx, &mut app, &shapes, "Session");
        assert_eq!(app.alert_draft.mode, AlertDraftMode::Session);
        let shapes = paint(&ctx, &mut app, vec![]);
        click(&ctx, &mut app, &shapes, "Pace");
        assert_eq!(
            app.draft_kind().unwrap(),
            AlertKind::Session { signal: tr_data::session_alerts::SessionSignal::Pace }
        );
        let shapes = paint(&ctx, &mut app, vec![]);
        click(&ctx, &mut app, &shapes, "WATCH");
        assert!(app.panes.iter().any(|p| matches!(p.kind, super::super::PaneKind::Alerts)));
    }
}
