//! Local planning estimates; no worker or trading actions.
use super::portfolio_stops::Store;
use egui::{Color32, RichText, Ui};
use std::collections::HashMap;
use tr_broker::{InstrumentKind, Portfolio, Position};
use tr_core::portfolio_risk::{self as engine, Holding, HoldingKind, PortfolioRisk, RiskInput, Status};
use tr_data::Quote;

#[derive(Default)]
pub(super) struct State {
    budgets: HashMap<String, String>,
    stops: HashMap<(String, String), String>,
    error: Option<String>,
}
fn positive(v: f64) -> bool {
    v.is_finite() && v > 0.0
}
fn holding(p: &Position, quotes: &HashMap<String, Quote>) -> Holding {
    // Quote records have no currency field; establish denomination from the key.
    // Options must never borrow their underlying's quote.
    let live = (p.kind != InstrumentKind::Option)
        .then(|| p.market_key())
        .flatten()
        .filter(|key| match p.kind {
            InstrumentKind::Stock | InstrumentKind::Etf | InstrumentKind::Fund => {
                key.ends_with(" US Equity") && p.currency == "USD"
            }
            InstrumentKind::Crypto => !p.currency.is_empty() && key.ends_with(&format!("{} Curncy", p.currency)),
            _ => false,
        })
        .and_then(|key| quotes.get(&key))
        .filter(|q| q.provenance.is_verified() && positive(q.last))
        .map(|q| q.last);
    Holding {
        account_id: p.account_id.clone(),
        symbol: p.symbol.clone(),
        kind: match p.kind {
            InstrumentKind::Stock => HoldingKind::Stock,
            InstrumentKind::Etf => HoldingKind::Etf,
            _ => HoldingKind::Other,
        },
        quantity: p.units,
        currency: p.currency.clone(),
        mark: live.or(p.price.filter(|v| positive(*v))),
    }
}
fn budget(text: &str) -> Result<Option<f64>, &'static str> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(Some)
        .ok_or("Invalid budget: enter a finite nonnegative amount")
}
fn verdict(risk: &PortfolioRisk, budget: Option<f64>, currency: &str) -> &'static str {
    if !risk.complete || risk.has_breaches() {
        "Budget not evaluated (partial/breached)"
    } else if currency.is_empty() || budget.is_none() {
        "Budget not evaluated"
    } else if risk.covered_risk > budget.unwrap() {
        "Over budget"
    } else {
        "Within budget"
    }
}
pub(super) fn panel(
    ui: &mut Ui,
    pf: &Portfolio,
    selected: Option<&str>,
    quotes: &HashMap<String, Quote>,
    store: &mut Store,
    state: &mut State,
) {
    ui.group(|ui| {
        ui.strong("Planned risk");
        let account =
            selected.and_then(|id| pf.account(id)).or_else(|| (pf.accounts.len() == 1).then(|| &pf.accounts[0]));
        let Some(account) = account else {
            ui.label("Select an account");
            return;
        };
        let currency = account.currency.trim();
        let draft = state
            .budgets
            .entry(account.id.clone())
            .or_insert_with(|| store.budget(&account.id).map(|v| v.to_string()).unwrap_or_default());
        ui.horizontal(|ui| {
            ui.label(format!("Budget ({})", if currency.is_empty() { "unknown currency" } else { currency }));
            ui.add(egui::TextEdit::singleline(draft).id_salt(("planning-budget", &account.id)).desired_width(100.0));
            if ui.button("Save").clicked() {
                state.error = match budget(draft) {
                    Err(error) => Some(error.into()),
                    Ok(value) => store
                        .set_budget(&account.id, value)
                        .err()
                        .map(|error| format!("Could not save budget: {error}")),
                };
            }
        });
        let parsed_budget = budget(draft);
        if let Err(error) = parsed_budget {
            ui.colored_label(Color32::RED, error);
        }
        // Exclude cash, broker cash equivalents, and closed positions explicitly.
        let holdings: Vec<_> = pf
            .positions
            .iter()
            .filter(|p| {
                p.account_id == account.id && p.units != 0.0 && p.kind != InstrumentKind::Cash && !p.cash_equivalent
            })
            .map(|p| holding(p, quotes))
            .collect();
        for h in &holdings {
            let key = (account.id.clone(), h.symbol.clone());
            let draft = state
                .stops
                .entry(key.clone())
                .or_insert_with(|| store.get(&key.0, &key.1).map(|v| v.to_string()).unwrap_or_default());
            ui.push_id(&key, |ui| {
                ui.horizontal(|ui| {
                    ui.label(&h.symbol);
                    ui.label("Planning stop");
                    ui.add(egui::TextEdit::singleline(draft).desired_width(85.0));
                    let save = ui.button("Save").clicked();
                    let clear = ui.button("Clear").clicked();
                    if save || clear {
                        let value = if clear {
                            Ok(None)
                        } else {
                            draft
                                .trim()
                                .parse::<f64>()
                                .ok()
                                .filter(|v| positive(*v))
                                .map(Some)
                                .ok_or("Planning stop must be finite and positive; use Clear to remove")
                        };
                        state.error = match value {
                            Err(e) => Some(e.into()),
                            Ok(value) => match store.set(&key.0, &key.1, value) {
                                Ok(()) => {
                                    if clear {
                                        draft.clear();
                                    }
                                    None
                                }
                                Err(e) => Some(format!("Could not save planning stop: {e}")),
                            },
                        };
                    }
                });
            });
        }
        let stops = store.stops();
        match engine::calculate(RiskInput {
            holdings: &holdings,
            stops: &stops,
            candidate: None,
            budget: parsed_budget.unwrap_or(None),
            account_filter: Some(&account.id),
            base_currency: currency,
        }) {
            Err(e) => {
                ui.colored_label(Color32::RED, format!("Planning estimate unavailable: {e:?}"));
            }
            Ok(risk) => {
                ui.label(format!("Known planned loss: {:.2} {}", risk.covered_risk, currency));
                let covered = risk.rows.iter().filter(|r| r.status == Status::Covered).count();
                let breached = risk.rows.iter().filter(|r| r.status == Status::Breached).count();
                ui.label(format!(
                    "Covered: {covered} · Uncovered: {} · Breached: {breached}",
                    risk.rows.len() - covered - breached
                ));
                if !risk.complete {
                    ui.strong("Partial estimate");
                }
                for row in &risk.rows {
                    match row.status {
                        Status::Breached => {
                            ui.label(
                                RichText::new(format!(
                                    "{}: Stop breached — planned loss excludes this position",
                                    row.holding.symbol
                                ))
                                .strong()
                                .color(Color32::RED),
                            );
                        }
                        Status::Covered => {
                            ui.label(format!("{}: Covered", row.holding.symbol));
                        }
                        Status::Uncovered(reason) => {
                            ui.label(format!("{}: Uncovered ({reason:?})", row.holding.symbol));
                        }
                    }
                }
                if let Ok(budget) = parsed_budget {
                    ui.strong(verdict(&risk, budget, currency));
                }
            }
        }
        if let Some(error) = &state.error {
            ui.colored_label(Color32::RED, error);
        }
        ui.label("Planning stops place no orders and do not guarantee a loss limit.");
    });
}

#[cfg(test)]
mod tests {
    use super::super::{TerminalApp, tests::test_app};
    use super::*;
    use tr_core::Provenance;

    #[test]
    fn adapter_keeps_signed_units_and_rejects_untrusted_prices() {
        let mut p = tr_broker::sample::portfolio().positions.remove(0);
        p.kind = InstrumentKind::Stock;
        p.symbol = "AAPL".into();
        p.currency = "USD".into();
        p.units = -2.0;
        p.price = Some(10.0);
        let key = p.market_key().unwrap();
        let mut quotes = HashMap::from([(
            key.clone(),
            Quote {
                provenance: Provenance::Sample,
                symbol: key.clone(),
                last: 20.0,
                prev_close: 20.0,
                ts: 0,
                spark: vec![],
                stats: Default::default(),
            },
        )]);
        assert_eq!(holding(&p, &quotes).mark, Some(10.0));
        assert_eq!(holding(&p, &quotes).quantity, -2.0);
        quotes.get_mut(&key).unwrap().provenance = Provenance::Real;
        assert_eq!(holding(&p, &quotes).mark, Some(20.0));
        p.currency = "EUR".into();
        assert_eq!(holding(&p, &quotes).mark, Some(10.0));
        for price in [None, Some(0.0), Some(f64::NAN)] {
            p.price = price;
            assert_eq!(holding(&p, &quotes).mark, None);
        }
        for kind in [
            InstrumentKind::Stock,
            InstrumentKind::Etf,
            InstrumentKind::Fund,
            InstrumentKind::Crypto,
            InstrumentKind::Option,
            InstrumentKind::Future,
            InstrumentKind::Bond,
            InstrumentKind::Cash,
            InstrumentKind::Other,
        ] {
            p.kind = kind;
            assert_eq!(
                holding(&p, &quotes).kind,
                match kind {
                    InstrumentKind::Stock => HoldingKind::Stock,
                    InstrumentKind::Etf => HoldingKind::Etf,
                    _ => HoldingKind::Other,
                }
            );
        }
        p.currency = "USD".into();
        p.kind = InstrumentKind::Stock;
        p.price = Some(10.0);
        quotes.get_mut(&key).unwrap().last = f64::NAN;
        assert_eq!(holding(&p, &quotes).mark, Some(10.0));
    }

    #[test]
    fn budget_verdict_requires_complete_unbreached_coverage() {
        assert_eq!(budget(""), Ok(None));
        assert_eq!(budget("0"), Ok(Some(0.0)));
        for invalid in ["bad", "NaN", "-1", "inf"] {
            assert!(budget(invalid).is_err());
        }
        let p = tr_broker::sample::portfolio().positions.remove(0);
        let mut h = holding(&p, &HashMap::new());
        h.kind = HoldingKind::Stock;
        h.currency = "USD".into();
        h.quantity = 2.0;
        h.mark = Some(10.0);
        let mut stops = engine::Stops::new();
        for (stop, expected) in [
            (None, "Budget not evaluated (partial/breached)"),
            (Some(10.0), "Budget not evaluated (partial/breached)"),
            (Some(5.0), "Over budget"),
            (Some(9.0), "Within budget"),
        ] {
            stops.clear();
            if let Some(stop) = stop {
                stops.insert((h.account_id.clone(), h.symbol.clone()), stop);
            }
            let risk = engine::calculate(RiskInput {
                holdings: std::slice::from_ref(&h),
                stops: &stops,
                candidate: None,
                budget: Some(5.0),
                account_filter: None,
                base_currency: "USD",
            })
            .unwrap();
            assert_eq!(verdict(&risk, Some(5.0), "USD"), expected);
            assert_ne!(verdict(&risk, Some(5.0), ""), "Within budget");
        }
    }

    #[test]
    fn portfolio_hook_ignores_symbol_filter_in_summary_and_positions() {
        let mut app = test_app();
        let ctx = egui::Context::default();
        let mut pf = tr_broker::sample::portfolio();
        pf.accounts.truncate(1);
        let account = pf.accounts[0].id.clone();
        pf.positions.retain(|p| p.account_id == account);
        assert!(!pf.positions.is_empty());
        let expected_symbol = pf.positions[0].symbol.clone();
        for view in [crate::command::PortView::Summary, crate::command::PortView::Positions] {
            let mut pane = super::super::broker::PortfolioPane::new(view);
            pane.symbol = Some("DOESNOTEXIST US Equity".into());
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 1600.0))),
                    ..Default::default()
                },
                |ui| {
                    super::super::portfolio_view::portfolio(
                        ui,
                        &mut pane,
                        &app.broker,
                        Some(&pf),
                        &app.quotes,
                        Some((&mut app.planning_stops, &mut app.planning_ui)),
                    );
                },
            );
            output.textures_delta.clear();
            let rendered = texts(&output.shapes);
            assert!(rendered.iter().any(|(t, _)| t == "Planned risk"));
            assert!(!rendered.iter().any(|(t, _)| t == "Select an account"));
            assert!(app.planning_ui.stops.contains_key(&(account.clone(), expected_symbol.clone())));
        }
    }

    fn paint(
        ctx: &egui::Context,
        app: &mut TerminalApp,
        pf: &Portfolio,
        selected: Option<&str>,
        events: Vec<egui::Event>,
    ) -> Vec<egui::epaint::ClippedShape> {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 1800.0))),
                events,
                ..Default::default()
            },
            |ui| panel(ui, pf, selected, &app.quotes, &mut app.planning_stops, &mut app.planning_ui),
        );
        output.textures_delta.clear();
        output.shapes
    }
    fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, egui::Pos2)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, egui::Pos2)>) {
            match shape {
                egui::Shape::Text(t) => out.push((t.galley.job.text.clone(), t.pos + t.galley.size() * 0.5)),
                egui::Shape::Vec(v) => {
                    for s in v {
                        walk(s, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = vec![];
        for shape in shapes {
            walk(&shape.shape, &mut out);
        }
        out
    }
    #[test]
    fn saved_planning_edits_reload_and_never_send_worker_requests() {
        let mut app = test_app();
        let (worker, mut requests) = crate::worker::Worker::test_channel();
        app.worker = worker;
        let ctx = egui::Context::default();
        let mut pf = tr_broker::sample::portfolio();
        let account = pf.accounts[0].id.clone();
        let other = pf.accounts[1].id.clone();
        let mut p = pf.positions[0].clone();
        p.account_id = account.clone();
        p.price = Some(10.0);
        p.units = 2.0;
        p.currency = pf.accounts[0].currency.clone();
        p.kind = InstrumentKind::Stock;
        p.cash_equivalent = false;
        let symbol = p.symbol.clone();
        pf.positions = vec![p.clone()];
        app.planning_ui.budgets.insert(account.clone(), "10000".into());
        app.planning_ui.stops.insert((account.clone(), symbol.clone()), "1".into());
        let shapes = paint(&ctx, &mut app, &pf, Some(&account), vec![]);
        assert_eq!(app.planning_stops.budget(&account), None);
        let save_budget = texts(&shapes).into_iter().find(|(t, _)| t == "Save").unwrap().1;
        let click_budget = |pressed| egui::Event::PointerButton {
            pos: save_budget,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        paint(&ctx, &mut app, &pf, Some(&account), vec![egui::Event::PointerMoved(save_budget), click_budget(true)]);
        paint(&ctx, &mut app, &pf, Some(&account), vec![click_budget(false)]);
        assert_eq!(app.planning_stops.budget(&account), Some(10000.0));
        app.planning_ui.budgets.insert(account.clone(), "invalid".into());
        paint(&ctx, &mut app, &pf, Some(&account), vec![egui::Event::PointerMoved(save_budget), click_budget(true)]);
        paint(&ctx, &mut app, &pf, Some(&account), vec![click_budget(false)]);
        assert_eq!(app.planning_stops.budget(&account), Some(10000.0));
        assert_eq!(app.planning_ui.error.as_deref(), Some("Invalid budget: enter a finite nonnegative amount"));
        app.planning_ui.budgets.insert(account.clone(), "10000".into());
        let shapes = paint(&ctx, &mut app, &pf, Some(&account), vec![]);
        let entry = texts(&shapes).into_iter().find(|(t, _)| t == "1").unwrap().1;
        let pointer_at = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        paint(&ctx, &mut app, &pf, Some(&account), vec![egui::Event::PointerMoved(entry), pointer_at(entry, true)]);
        paint(
            &ctx,
            &mut app,
            &pf,
            Some(&account),
            vec![
                pointer_at(entry, false),
                egui::Event::Key {
                    key: egui::Key::End,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::Text("1".into()),
            ],
        );
        let save = texts(&shapes).into_iter().filter(|(t, _)| t == "Save").nth(1).unwrap().1;
        let pointer = |pressed| egui::Event::PointerButton {
            pos: save,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        paint(&ctx, &mut app, &pf, Some(&account), vec![egui::Event::PointerMoved(save), pointer(true)]);
        paint(&ctx, &mut app, &pf, Some(&account), vec![pointer(false)]);
        assert_eq!(app.planning_stops.get(&account, &symbol), Some(11.0));
        let dir = app.planning_stops.path.parent().unwrap().to_owned();
        assert!(requests.try_recv().is_err(), "planning edits must send no requests");
        app = test_app();
        let (worker, receiver) = crate::worker::Worker::test_channel();
        app.worker = worker;
        requests = receiver;
        app.planning_stops = Store::load(dir.clone());
        let rendered = texts(&paint(&ctx, &mut app, &pf, Some(&account), vec![]));
        assert!(rendered.iter().any(|(t, _)| t.contains("Stop breached — planned loss excludes this position")));
        assert!(!rendered.iter().any(|(t, _)| t == "Within budget"));
        assert_eq!(app.planning_ui.stops[&(account.clone(), symbol.clone())], "11");
        assert_eq!(app.planning_ui.budgets[&account], "10000");
        assert_eq!(app.planning_stops.budget(&account), Some(10000.0));
        p.account_id = other.clone();
        pf.positions.push(p);
        paint(&ctx, &mut app, &pf, Some(&other), vec![]);
        assert_eq!(app.planning_ui.stops[&(other.clone(), symbol.clone())], "");
        assert_eq!(app.planning_stops.get(&other, &symbol), None);
        assert_eq!(app.planning_ui.budgets[&other], "");
        let rendered = texts(&paint(&ctx, &mut app, &pf, Some(&other), vec![]));
        assert!(rendered.iter().any(|(t, _)| t == "Partial estimate"));
        assert!(!rendered.iter().any(|(t, _)| t == "Within budget"));
        let rendered = texts(&paint(&ctx, &mut app, &pf, None, vec![]));
        assert!(rendered.iter().any(|(t, _)| t == "Select an account"));
        let shapes = paint(&ctx, &mut app, &pf, Some(&account), vec![]);
        let clear = texts(&shapes).into_iter().find(|(t, _)| t == "Clear").unwrap().1;
        paint(&ctx, &mut app, &pf, Some(&account), vec![egui::Event::PointerMoved(clear), pointer_at(clear, true)]);
        paint(&ctx, &mut app, &pf, Some(&account), vec![pointer_at(clear, false)]);
        assert_eq!(Store::load(dir.clone()).get(&account, &symbol), None);
        pf.accounts.truncate(1);
        let rendered = texts(&paint(&ctx, &mut app, &pf, None, vec![]));
        assert!(!rendered.iter().any(|(t, _)| t == "Select an account"));
        assert!(requests.try_recv().is_err(), "planning edits must send no requests");
        app.planning_ui.budgets.insert(account.clone(), "  ".into());
        let shapes = paint(&ctx, &mut app, &pf, Some(&account), vec![]);
        let save = texts(&shapes).into_iter().find(|(t, _)| t == "Save").unwrap().1;
        paint(&ctx, &mut app, &pf, Some(&account), vec![egui::Event::PointerMoved(save), pointer_at(save, true)]);
        paint(&ctx, &mut app, &pf, Some(&account), vec![pointer_at(save, false)]);
        assert_eq!(Store::load(dir.clone()).budget(&account), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
