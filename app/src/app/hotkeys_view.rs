//! Native keyboard-shortcut table (click a binding to record a new one) and the Help pane.

use egui::{Align2, Color32, Id, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use tr_core::indicators::registry::{self, Category};

use super::TerminalApp;
use crate::command::IndicatorSpec;
use crate::prefs::{Action, Keymap};
use crate::theme::{self, amber, dp, mono, muted, text, ui_font};

/// Paint key caps right-aligned to `right`, vertically centred on `cy`. Returns their width.
pub fn paint_key_caps(p: &egui::Painter, right: f32, cy: f32, tokens: &[String]) -> f32 {
    let font = mono(11.5);
    let galleys: Vec<_> = tokens.iter().map(|t| p.layout_no_wrap(t.clone(), font.clone(), text())).collect();
    let widths: Vec<f32> = galleys.iter().map(|g| (g.size().x + 10.0).max(22.0)).collect();
    let total = widths.iter().sum::<f32>() + 4.0 * (widths.len().saturating_sub(1)) as f32;
    let mut x = right - total;
    for (g, w) in galleys.into_iter().zip(widths) {
        let cap = Rect::from_min_size(pos2(x, cy - 11.0), vec2(w, 22.0));
        // A key: raised face with a darker lower edge.
        p.rect_filled(cap.translate(vec2(0.0, 1.5)), 5.0, Color32::from_black_alpha(40));
        p.rect_filled(cap, 5.0, theme::control_hover());
        p.rect_stroke(cap, 5.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
        p.galley(pos2(cap.center().x - g.size().x / 2.0, cy - g.size().y / 2.0), g, text());
        x += w + 4.0;
    }
    total
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(dp(14.0));
    ui.label(RichText::new(title).font(ui_font(15.0)).color(text()));
    ui.add_space(dp(4.0));
}

fn para(ui: &mut Ui, s: &str) {
    ui.label(RichText::new(s).font(ui_font(12.5)).color(muted()));
}

fn chip(ui: &mut Ui, code: &str) -> egui::Response {
    ui.label(RichText::new(format!(" {code} ")).font(mono(12.0)).color(amber()).background_color(theme::control()))
}

fn chips(ui: &mut Ui, codes: &[&str]) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for c in codes {
            chip(ui, c);
        }
    });
}

/// Command chips with descriptions: a two-column table when there is room, otherwise
/// each command's chips above its (wrapped) description.
fn command_table(ui: &mut Ui, id: &str, rows: &[(&[&str], &str)]) {
    if ui.available_width() < 600.0 {
        for (codes, desc) in rows {
            chips(ui, codes);
            ui.add(egui::Label::new(RichText::new(*desc).font(ui_font(12.5)).color(text())).wrap());
            ui.add_space(dp(5.0));
        }
        return;
    }
    let desc_w = (ui.available_width() - 330.0).max(200.0);
    egui::Grid::new(id).num_columns(2).min_col_width(300.0).spacing([18.0, 6.0]).striped(true).show(ui, |ui| {
        for (codes, desc) in rows {
            chips(ui, codes);
            ui.scope(|ui| {
                ui.set_max_width(desc_w);
                ui.add(egui::Label::new(RichText::new(*desc).font(ui_font(12.5)).color(text())).wrap());
            });
            ui.end_row();
        }
    });
}

impl TerminalApp {
    /// Grouped, searchable table of every action and its binding. Click a binding, then
    /// press the new keys (Esc cancels). Returns whether the keymap changed.
    pub(super) fn hotkey_table(&mut self, ui: &mut Ui) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.hotkey_filter).hint_text("Search shortcuts").desired_width(220.0),
            );
            if ui.button("Restore Defaults").clicked() {
                self.prefs.hotkeys = Keymap::default();
                self.rebinding = None;
                changed = true;
            }
        });
        let q = self.hotkey_filter.trim().to_lowercase();
        let defaults = Keymap::default();
        let row_h = dp(32.0);
        let time = ui.input(|i| i.time);
        for cat in Action::CATEGORIES {
            let actions: Vec<Action> = Action::ALL
                .into_iter()
                .filter(|a| a.category() == cat)
                .filter(|a| {
                    q.is_empty()
                        || a.label().to_lowercase().contains(&q)
                        || self.prefs.hotkeys.chord(*a).is_some_and(|c| c.display().to_lowercase().contains(&q))
                })
                .collect();
            if actions.is_empty() {
                continue;
            }
            ui.add_space(dp(10.0));
            ui.label(RichText::new(cat).font(ui_font(12.0)).color(muted()));
            ui.add_space(2.0);
            let w = ui.available_width().min(ui.clip_rect().right() - ui.cursor().left() - 4.0).max(260.0);
            let (card, _) = ui.allocate_exact_size(vec2(w, row_h * actions.len() as f32), Sense::hover());
            let p = ui.painter_at(card.expand(2.0));
            p.rect_filled(card, 8.0, theme::card());
            p.rect_stroke(card, 8.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
            for (k, action) in actions.iter().copied().enumerate() {
                let row = Rect::from_min_size(pos2(card.left(), card.top() + k as f32 * row_h), vec2(w, row_h));
                if k > 0 {
                    p.hline(row.x_range().shrink(12.0), row.top(), Stroke::new(1.0, theme::border()));
                }
                let hovered = ui.rect_contains_pointer(row);
                let bw = (row.width() * 0.45).clamp(120.0, 210.0);
                let binding = Rect::from_min_max(
                    pos2(row.right() - bw, row.top() + 3.0),
                    pos2(row.right() - 8.0, row.bottom() - 3.0),
                );
                let label_clip = Rect::from_min_max(row.min, pos2(binding.left() - 4.0, row.bottom()));
                p.with_clip_rect(label_clip).text(
                    pos2(row.left() + 12.0, row.center().y),
                    Align2::LEFT_CENTER,
                    action.label(),
                    ui_font(13.0),
                    text(),
                );
                let resp = ui
                    .interact(binding, Id::new(("hotkey", action)), Sense::click())
                    .on_hover_text("Click, then press the new shortcut (Esc cancels)");
                let capturing = self.rebinding == Some(action);
                if capturing {
                    let pulse = 0.55 + 0.45 * ((time * 4.0).sin() as f32).abs();
                    p.rect_filled(binding, 6.0, theme::accent_wash());
                    p.rect_stroke(
                        binding,
                        6.0,
                        Stroke::new(1.5, theme::accent().gamma_multiply(pulse)),
                        StrokeKind::Inside,
                    );
                    p.text(binding.center(), Align2::CENTER_CENTER, "Type shortcut…", ui_font(12.0), theme::accent());
                    ui.ctx().request_repaint();
                } else {
                    if resp.hovered() {
                        p.rect_filled(binding, 6.0, theme::control());
                    }
                    match self.prefs.hotkeys.chord(action) {
                        Some(c) => {
                            paint_key_caps(&p, binding.right() - 6.0, binding.center().y, &c.tokens());
                        }
                        None => {
                            p.text(
                                pos2(binding.right() - 8.0, binding.center().y),
                                Align2::RIGHT_CENTER,
                                "Not set",
                                ui_font(12.0),
                                muted(),
                            );
                        }
                    }
                }
                if resp.clicked() {
                    self.rebinding = if capturing { None } else { Some(action) };
                }
                // Row actions on hover: reset to default, clear.
                if hovered && !capturing {
                    let mut x = binding.left() - 6.0;
                    let mut small = |label: &str, tip: &str| {
                        let g = p.layout_no_wrap(label.to_string(), ui_font(11.5), theme::accent());
                        let r = Rect::from_min_size(
                            pos2(x - g.size().x - 10.0, row.center().y - 10.0),
                            vec2(g.size().x + 10.0, 20.0),
                        );
                        x = r.left() - 4.0;
                        let resp =
                            ui.interact(r, Id::new(("hotkey-op", action, label)), Sense::click()).on_hover_text(tip);
                        if resp.hovered() {
                            p.rect_filled(r, 5.0, theme::control());
                        }
                        p.galley(pos2(r.left() + 5.0, row.center().y - g.size().y / 2.0), g, theme::accent());
                        resp.clicked()
                    };
                    let current = self.prefs.hotkeys.0.get(&action).cloned();
                    let default = defaults.0.get(&action).cloned();
                    if current.is_some() && small("Clear", "Remove this shortcut") {
                        self.prefs.hotkeys.0.remove(&action);
                        changed = true;
                    }
                    if current != default
                        && let Some(d) = default
                        && small("Reset", "Restore the default shortcut")
                    {
                        if let Some(c) = crate::prefs::Chord::parse(&d) {
                            self.prefs.hotkeys.bind(action, c);
                        }
                        changed = true;
                    }
                }
            }
        }
        ui.add_space(dp(6.0));
        ui.label(
            RichText::new(
                "Shortcuts with ⌘/Ctrl or Alt work even while typing a command. Plain letters start commands \
                 (type-anywhere), so bind panels to chords such as ⌘1 rather than single letters.",
            )
            .font(ui_font(11.5))
            .color(muted()),
        );
        changed
    }

    /// The Help pane: commands and an editable shortcut table.
    pub(super) fn help_panel(&mut self, ui: &mut Ui) {
        let mut changed = false;
        let width = ui.available_width() - 14.0;
        egui::ScrollArea::vertical().id_salt("help").auto_shrink([false, false]).show(ui, |ui| {
            // Content never grows wider than the pane (long rows wrap or truncate instead).
            ui.set_max_width(width);
            ui.label(RichText::new("Commands & Hotkeys").font(ui_font(19.0)).color(text()));
            para(
                ui,
                "Type anywhere to start a command: <SECURITY> [<FUNCTION>] then Enter (<GO>). Matches across \
                 every asset class appear as you type; Up/Down selects, Tab completes.",
            );

            section(ui, "Securities");
            command_table(
                ui,
                "help-sec",
                &[
                    (&["AAPL", "AAPL US EQUITY"], "US equities and ETFs"),
                    (&["SPX INDEX"], "Indices"),
                    (&["BTCUSD CRNCY", "EURUSD CRNCY"], "Crypto (live) and FX"),
                    (&["US10Y GOVT"], "US Treasury yields"),
                    (&["CL1 CMDTY", "GC1 CMDTY"], "Futures and commodities"),
                    (&["SECF gold"], "Search securities"),
                ],
            );

            section(ui, "Panels");
            command_table(
                ui,
                "help-panels",
                &[
                    (&["HIST"], "Price history: bundled universe, download, refresh, cancel, resume and retry failed"),
                    (&["HIST ADD ALL"], "Archive full daily Yahoo history for the bundled research universe"),
                    (&["UNIV"], "Whats-News archive: last bar date, STALE and MISSING coverage"),
                    (
                        &["SCAN DARVAS_BREAKOUT", "SCAN NEAR_HIGH"],
                        "Scan archived bars; Enter opens the linked archive chart (REVIEW for daily+weekly)",
                    ),
                    (&["WEI"], "World markets overview"),
                    (&["GC"], "US Treasury yield curve"),
                    (&["N", "AAPL CN", "TOP"], "Watchlist / scanner-leader news; AAPL CN for one name; TOP for the market"),
                    (&["OB", "BTCUSD CRNCY OB 50"], "Order book depth"),
                    (&["OMON", "AAPL OMON", "SPX INDEX OMON"], "Option chain: calls/puts by strike, IV, greeks"),
                    (&["CONG", "NVDA CONG"], "Congress trades feed / who traded a ticker"),
                    (&["POL", "POL PELOSI", "TRACK PELOSI"], "Politicians, profiles and alerts"),
                    (
                        &[
                            "ALERTS",
                            "AAPL ALERT 150",
                            "AAPL ALERT VWAP",
                            "AAPL ALERT RVOL 2",
                            "AAPL ALERT PATH",
                            "AAPL ALERT PACE",
                            "AAPL ALERT GAP",
                            "AAPL ALERT XTRM",
                            "AAPL ALERT RPATH",
                            "NVDA ALERT SETUP NEAR_HIGH",
                        ],
                        "Price, VWAP, RVOL, session path/pace/gap/extreme/relative, and setup alerts",
                    ),
                    (&["UNALERT AAPL"], "Remove price/setup alerts for a name"),
                    (&["WL", "WL ADD SOLUSD CRNCY"], "Watchlist"),
                    (
                        &["EARNINGS", "EARN", "ECAL"],
                        "Earnings calendar for the selected watchlist: provider dates only, click a row to open the chart",
                    ),
                    (
                        &["ISCAN", "INTRADAY"],
                        "Intraday scan of the selected watchlist: session RVOL, close versus VWAP, and return versus SPY from verified 5-minute bars",
                    ),
                    (
                        &["IDESK", "IDASH", "AAPL IDESK"],
                        "Intraday desk: 5-minute chart, ISCAN, breadth, and volume pace for one name, saved as the Intraday layout",
                    ),
                    (
                        &["DESK", "AAPL DESK", "NEW DESK"],
                        "PM Desk: watchlist leaders, group performance, position size (not PORT)",
                    ),
                    (
                        &["JOURNAL", "AAPL JOURNAL", "AAPL NOTE 2026-10-24"],
                        "Trade journal: setup + checklist + planned $ risk; record $ and R; notes on chart dates",
                    ),
                    (
                        &["STATS", "AAPL STATS", "AAPL COND RSI(14) < 30"],
                        "Historical risk/returns; what followed low RSI or price above a moving average",
                    ),
                    (
                        &["BACKTEST", "AAPL BACKTEST", "AAPL BT ADTREND"],
                        "Paper backtest vs buy-and-hold: costs, next-open fills, chronological holdout, completed-trade win rate",
                    ),
                    (
                        &["IBT", "AAPL IBT", "AAPL IBT VWAP", "AAPL IBT OR 15m"],
                        "Paper intraday backtest: session VWAP and opening range vs holding the cash session",
                    ),
                    (
                        &["LOOKALIKE", "AAPL LOOKALIKE", "AAPL KNN 2024-01-15", "GOTO 2024-01-15"],
                        "Historical lookalikes: nearest past conditions, per-horizon counts, jump to neighbor dates",
                    ),
                    (
                        &["EXPORT", "EXPORT SCAN", "EXPORT JOURNAL", "EXPORT HIST", "EXPORT BACKTEST", "EXPORT LOOKALIKE", "BACKUP", "RESTORE"],
                        "Export scans/history/journal/backtests/lookalikes with source and date; back up or restore workspace (no credentials)",
                    ),
                    (&["PREFS"], "Settings: theme, fonts, density, hotkeys, layouts, backup"),
                    (&["UPDATE"], "Check for updates"),
                ],
            );

            section(ui, "Brokerage (SnapTrade)");
            command_table(
                ui,
                "help-broker",
                &[
                    (&["PORT", "F7"], "Portfolio: total value, cash, unrealised P&L, allocation, top holdings"),
                    (&["ACCT"], "Accounts and balances (cash, buying power, sync time)"),
                    (&["POS", "F8", "AAPL POS"], "Positions with live P&L (for one security)"),
                    (&["ORD", "F9", "AAPL ORD"], "Orders from the last 30 days (open or all)"),
                    (&["ACTV"], "Activity: trades, dividends, interest, transfers"),
                    (
                        &["PORT REFRESH", "PORT CONNECT", "PORT SETUP"],
                        "Reload · connect a broker in the browser · keys",
                    ),
                ],
            );

            section(ui, "Trading (off until enabled in Settings › Trading)");
            command_table(
                ui,
                "help-trading",
                &[
                    (&["BUY AAPL 10", "SELL MSFT 5"], "Order ticket, prefilled (preview and confirm before sending)"),
                    (&["BUY AAPL 10 LMT 190", "@190"], "Limit order"),
                    (&["SELL TSLA 3 STP 250 LMT 245"], "Stop or stop-limit order"),
                    (&["BUY NVDA $500"], "Dollar amount (market, day; broker permitting)"),
                    (&["TRADE", "AAPL BUY"], "Open the ticket"),
                ],
            );

            section(ui, "Charts");
            command_table(
                ui,
                "help-chart",
                &[
                    (&["1", "5", "1H", "D", "1W"], "Interval"),
                    (&["1D", "5D", "YTD", "1Y", "ALL"], "Quick range"),
                    (&["CANDLE", "BAR", "LINE", "AREA", "LOG"], "Chart type and log scale"),
                    (&["SMA 50", "RSI", "IND"], "Indicators (see below)"),
                    (&["CMP MSFT"], "Compare, rebased to the main symbol"),
                    (
                        &["ICMP", "AAPL ICMP MSFT NVDA"],
                        "Intraday compare: percent from each name's 09:30 open, with SPY",
                    ),
                    (
                        &["SPATH", "AAPL SPATH 20 15m"],
                        "Today versus the typical path of prior sessions, percent from each 09:30 open",
                    ),
                    (
                        &["BRD", "AAPL BRD MSFT NVDA"],
                        "Share of the list at or above each 09:30 open and session VWAP",
                    ),
                    (
                        &["VPACE", "AAPL VPACE 20 15m"],
                        "Cumulative volume versus the usual pace of prior sessions",
                    ),
                    (
                        &["GFILL", "AAPL GFILL 40 15m"],
                        "Today's open versus the prior cash close, and how often that gap had filled",
                    ),
                    (
                        &["XTRM", "AAPL XTRM 20 15m"],
                        "When the session high and low print, against completed prior sessions",
                    ),
                    (
                        &["RPATH", "AAPL RPATH 20 15m"],
                        "Percent from the 09:30 open minus SPY, against prior sessions",
                    ),
                    (
                        &["BRIEF", "SBRIEF", "AAPL BRIEF"],
                        "Session brief: path, pace, gap, extreme, and versus SPY for one name. Not a chart",
                    ),
                    (&["TL", "HL", "VL", "FIB", "MAGNET"], "Drawing tools and snapping"),
                    (&["CLEAR DRAW", "UNDO DRAW"], "Remove all drawings on this name, or undo the last delete"),
                    (&["REVIEW", "AAPL REVIEW"], "Linked daily + weekly archive charts for candidate review"),
                    (&["LATEST", "END"], "Jump to the latest candle (keeps zoom)"),
                    (&["BOX", "RS", "VCTX"], "Toggle box levels, 21-bar return, volume context"),
                    (&["ADTREND", "ATREND"], "Adaptive Trend bands, regime flips, MRT/LRT stops (verified bars)"),
                ],
            );
            para(
                ui,
                "Wheel zooms, drag pans, drag an axis to stretch, double-click resets, End jumps to the latest candle. Right-click for the chart menu. Daily VWAP is labelled Monthly VWAP because it resets each calendar month.",
            );

            section(ui, &format!("Indicators ({})", registry::all().len()));
            command_table(
                ui,
                "help-ind",
                &[
                    (&["RSI", "SMA 200", "BB 20 2.5"], "Toggle an indicator; numbers set its parameters"),
                    (&["IND", "IND VOLUME"], "Indicator picker: search by name, code or category"),
                    (&["AAPL ADX"], "Load a security and add the indicator"),
                    (&["CLEAR"], "Remove all indicators"),
                ],
            );
            para(
                ui,
                "Missing parameters take their defaults and out-of-range ones are clamped. Moving averages can be \
                 added several times with different lengths. Hover a code for its name and defaults.",
            );
            for cat in Category::ALL {
                ui.add_space(dp(4.0));
                ui.label(RichText::new(cat.label()).font(ui_font(12.5)).color(text()));
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for d in registry::all().iter().filter(|d| d.category == cat) {
                        chip(ui, d.code).on_hover_text(format!("{} · {}", d.name, IndicatorSpec::of(d).label()));
                    }
                });
            }

            section(ui, "Workspace");
            command_table(
                ui,
                "help-ws",
                &[
                    (&["NEW BOOK", "NEW NEWS AAPL"], "Add a panel beside the focused one"),
                    (&["SPLIT", "SPLIT DOWN NEWS"], "Split the focused pane"),
                    (&["POP"], "Pop the pane out to its own window (close it to dock back)"),
                    (&["LINK BLUE", "LINK OFF"], "Colour link group: one security drives linked panes"),
                    (&["TITLE Desk", "CLOSE"], "Rename or close the focused tab"),
                    (&["SIDEBAR RIGHT", "SIDEBAR LEFT", "SIDEBAR"], "Sidebar side, or show/hide (also the View menu)"),
                    (
                        &["THEME NORD", "THEME NEXT", "THEME LIST"],
                        "Switch theme (⌘⌥T cycles; THEME SYSTEM follows the OS)",
                    ),
                    (
                        &["THEME EDIT", "THEME COMPACT", "THEME MONO"],
                        "Theme editor, or adjust the active theme's density and font",
                    ),
                    (&["LAYOUT THEME ON"], "Save the current theme with each layout"),
                ],
            );
            para(
                ui,
                "Drag a tab, or the empty part of a pane's header, onto another pane: the compass shows where it \
                 lands (centre adds a tab, arrows split), or drop at a window edge. Drag dividers to resize; \
                 double-click one to equalise.",
            );

            section(ui, "Saved layouts");
            command_table(
                ui,
                "help-layouts",
                &[
                    (&["LAYOUT Macro"], "Switch to a saved layout"),
                    (&["LAYOUT SAVE", "LAYOUT SAVE Desk"], "Save (as)"),
                    (&["LAYOUT NEW Rates"], "Start a new layout"),
                    (&["LAYOUT RENAME", "LAYOUT DUP", "LAYOUT DEL"], "Rename, duplicate, delete"),
                    (&["LAYOUT EXPORT", "LAYOUT IMPORT ~/x.json"], "Share layouts as JSON"),
                    (&["LAYOUT 1", "2", "2H", "3", "4", "6"], "Preset grids (panels are kept as tabs)"),
                ],
            );

            section(ui, "Keyboard shortcuts");
            changed = self.hotkey_table(ui);

            section(ui, "Data");
            para(
                ui,
                "Crypto: Binance (Coinbase fallback) · Treasuries: Treasury.gov (FRED) · FX, indices, futures, \
                 ETFs and stocks: Yahoo, or Alpaca with APCA_API_KEY_ID/APCA_API_SECRET_KEY · News: Yahoo and \
                 CoinDesk · Congress: Senate eFD and House Clerk. TR_PROVIDER=sample runs offline.",
            );
            ui.add_space(dp(12.0));
        });
        if changed {
            self.save_prefs();
        }
    }
}
