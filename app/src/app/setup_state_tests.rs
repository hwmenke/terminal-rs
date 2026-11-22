//! Screens that need a ticker open into a neutral chooser — never a red command-style error.

use super::PaneKind;
use super::tests::test_app;
use crate::theme;
use crate::widgets::painted_texts;
use egui::{Rect, pos2, vec2};

fn render(app: &mut super::TerminalApp, f: impl Fn(&mut super::TerminalApp, &mut egui::Ui, crate::dock::PaneId)) -> Vec<(String, egui::Color32)> {
    let id = app.focus;
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 600.0))),
            ..Default::default()
        },
        |ui| f(app, ui, id),
    );
    out.textures_delta.clear();
    painted_texts(&out)
}

fn assert_needs_input(name: &str, texts: &[(String, egui::Color32)]) {
    let has = |s: &str| texts.iter().any(|(t, _)| t == s);
    assert!(has("Choose a ticker"), "{name}: {texts:?}");
    assert!(has("CHOOSE") && has("Use ticker"), "{name}: chooser and action missing");
    // No command-style error text, and nothing in the error / direction colours.
    assert!(texts.iter().all(|(t, _)| !t.contains("needs a ticker")), "{name}: command-style error");
    assert!(
        texts.iter().all(|(_, c)| *c != theme::down() && *c != theme::alert() && *c != theme::up()),
        "{name}: red/green text in a setup state"
    );
}

#[test]
fn every_ticker_screen_without_a_focus_renders_needs_input() {
    let mut app = test_app();
    assert!(app.focused_symbol().is_none());

    app.open_intraday_compare(None, "");
    assert!(matches!(app.panes.last().unwrap().kind, PaneKind::IntradayCompare(_)));
    let t = render(&mut app, |a, ui, id| {
        a.compare_pane(ui, id);
    });
    assert_needs_input("Session compare", &t);

    app.open_volume_pace(None, "");
    let t = render(&mut app, |a, ui, id| {
        a.volume_pace_pane(ui, id);
    });
    assert_needs_input("Volume pace", &t);

    app.open_gap_fill(None, "");
    let t = render(&mut app, |a, ui, id| {
        a.gap_fill_pane(ui, id);
    });
    assert_needs_input("Gap fill", &t);

    app.open_session_extreme(None, "");
    let t = render(&mut app, |a, ui, id| {
        a.session_extreme_pane(ui, id);
    });
    assert_needs_input("Session extreme", &t);

    app.open_lookalike(None, "");
    let t = render(&mut app, |a, ui, id| {
        a.lookalike_pane(ui, id);
    });
    assert_needs_input("Lookalikes", &t);
}

#[test]
fn opening_without_a_ticker_leaves_no_stored_error() {
    let mut app = test_app();
    app.open_intraday_compare(None, "");
    app.open_volume_pace(None, "");
    app.open_gap_fill(None, "");
    app.open_session_extreme(None, "");
    app.open_lookalike(None, "");
    for p in &app.panes {
        let err = match &p.kind {
            PaneKind::IntradayCompare(x) => &x.error,
            PaneKind::VolumePace(x) => &x.error,
            PaneKind::GapFill(x) => &x.error,
            PaneKind::SessionExtreme(x) => &x.error,
            PaneKind::Lookalike(x) => &x.error,
            _ => continue,
        };
        assert!(err.is_none(), "{err:?}");
    }
}

/// (symbol, generation, loading, draft) of the last pane, whatever its kind.
fn chooser_state(app: &mut super::TerminalApp) -> (String, u64, bool, String) {
    match &mut app.panes.last_mut().unwrap().kind {
        PaneKind::IntradayCompare(p) => (p.symbol.clone(), p.generation, p.loading, p.ticker_draft.clone()),
        PaneKind::VolumePace(p) => (p.symbol.clone(), p.generation, p.loading, p.ticker_draft.clone()),
        PaneKind::GapFill(p) => (p.symbol.clone(), p.generation, p.loading, p.ticker_draft.clone()),
        PaneKind::SessionExtreme(p) => (p.symbol.clone(), p.generation, p.loading, p.ticker_draft.clone()),
        PaneKind::Lookalike(p) => (p.symbol.clone(), p.generation, p.loading, p.ticker_draft.clone()),
        _ => unreachable!(),
    }
}

fn type_into_draft(app: &mut super::TerminalApp, text: &str) {
    // What the chooser's text box does with each keystroke: edit its own buffer.
    match &mut app.panes.last_mut().unwrap().kind {
        PaneKind::IntradayCompare(p) => p.ticker_draft = text.into(),
        PaneKind::VolumePace(p) => p.ticker_draft = text.into(),
        PaneKind::GapFill(p) => p.ticker_draft = text.into(),
        PaneKind::SessionExtreme(p) => p.ticker_draft = text.into(),
        PaneKind::Lookalike(p) => p.ticker_draft = text.into(),
        _ => unreachable!(),
    }
}

#[test]
fn chooser_survives_typing_and_requests_nothing_until_committed() {
    type Draw = fn(&mut super::TerminalApp, &mut egui::Ui, crate::dock::PaneId);
    let draws: [(&str, fn(&mut super::TerminalApp), Draw); 5] = [
        ("Session compare", |a| a.open_intraday_compare(None, ""), |a, ui, id| {
            a.compare_pane(ui, id);
        }),
        ("Volume pace", |a| a.open_volume_pace(None, ""), |a, ui, id| {
            a.volume_pace_pane(ui, id);
        }),
        ("Gap fill", |a| a.open_gap_fill(None, ""), |a, ui, id| {
            a.gap_fill_pane(ui, id);
        }),
        ("Session extreme", |a| a.open_session_extreme(None, ""), |a, ui, id| {
            a.session_extreme_pane(ui, id);
        }),
        ("Lookalikes", |a| a.open_lookalike(None, ""), |a, ui, id| {
            a.lookalike_pane(ui, id);
        }),
    ];
    for (name, open, draw) in draws {
        let mut app = test_app();
        open(&mut app);
        let _ = render(&mut app, draw);
        for typed in ["A", "AA"] {
            type_into_draft(&mut app, typed);
            let t = render(&mut app, draw);
            assert_needs_input(name, &t);
            let (symbol, generation, loading, draft) = chooser_state(&mut app);
            assert!(symbol.is_empty(), "{name}: typing committed {symbol:?}");
            assert_eq!((generation, loading), (0, false), "{name}: a request was sent while typing");
            assert_eq!(draft, typed);
        }
    }
}

#[test]
fn a_bar_size_typo_is_a_check_inputs_card_not_a_failed_run() {
    let mut app = test_app();
    app.open_intraday_compare(Some("AAPL"), "");
    if let PaneKind::IntradayCompare(p) = &mut app.panes.last_mut().unwrap().kind {
        // Opening may already have requested with the default bar size; start from a clean slate.
        p.tf = "abc".into();
        p.loading = false;
        p.generation = 0;
    }
    let t = render(&mut app, |a, ui, id| {
        a.compare_pane(ui, id);
    });
    let has = |s: &str| t.iter().any(|(x, _)| x == s);
    assert!(has("Check the inputs") && has("CHOOSE"), "{t:?}");
    assert!(!has("FAILED") && !has("Could not complete this analysis"), "{t:?}");
    assert!(t.iter().all(|(_, c)| *c != theme::alert()), "typo painted in the alert colour");
    let PaneKind::IntradayCompare(p) = &app.panes.last().unwrap().kind else { unreachable!() };
    assert!(p.error.is_none() && p.input_error.is_some());
    assert_eq!(p.generation, 0, "no request for an unparsable query");
}

#[test]
fn needs_input_offers_watchlist_shortcuts() {
    let mut app = test_app();
    app.watchlist = vec!["AAPL US Equity".into()];
    app.open_gap_fill(None, "");
    let t = render(&mut app, |a, ui, id| {
        a.gap_fill_pane(ui, id);
    });
    assert!(t.iter().any(|(s, _)| s == "AAPL"), "{t:?}");
}
