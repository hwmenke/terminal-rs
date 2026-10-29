//! Pane content for every panel kind.

use std::time::Instant;

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, Ui, UiBuilder, pos2, vec2};
use tr_data::OrderBook;

use super::{PaneKind, TerminalApp};
use crate::congress_view::{self, Action};
use crate::markets_view;
use crate::theme::{self, amber, dp, mono, muted, text, ui_font};
use crate::widgets;
use crate::worker::Request;

impl TerminalApp {
    /// Draw pane `i` into `rect`. Returns its actions and whether it was clicked.
    pub(super) fn render_content(&mut self, ui: &mut Ui, i: usize, rect: Rect, focused: bool) -> (Vec<Action>, bool) {
        let mut actions = Vec::new();
        let id = self.panes[i].id;
        let dragging = self.drag.is_some_and(|d| d.active);
        if let PaneKind::Chart(c) = &mut self.panes[i].kind {
            c.hover_enabled = !dragging;
            if focused {
                self.watch_ui.active_chart = Some(id);
            }
            let clicked = c.show(ui, rect, focused, &self.worker);
            actions.append(&mut c.actions);
            return (actions, clicked);
        }
        let freshness = self.congress_freshness();
        let has_leaders = self.panes.iter().any(|p| matches!(&p.kind, PaneKind::Research(r) if r.results.is_some()));
        let inner = rect.shrink2(vec2(dp(8.0), dp(4.0)));
        let mut child = ui.new_child(UiBuilder::new().max_rect(inner).id_salt(("pane", id)));
        child.set_clip_rect(inner.intersect(ui.clip_rect()));
        let ui = &mut child;
        match &mut self.panes[i].kind {
            PaneKind::Feed { filter } => actions.extend(congress_view::feed(ui, &self.latest, filter, &freshness)),
            PaneKind::Ticker { symbol, trades, loading } => {
                if *loading {
                    ui.label(RichText::new("Loading…").color(muted()));
                } else {
                    actions.extend(congress_view::ticker_trades(ui, symbol, trades));
                }
            }
            PaneKind::Profile { query, data, loading } => {
                actions.extend(congress_view::profile(ui, query, data.as_deref(), *loading))
            }
            PaneKind::Politicians => {
                actions.extend(congress_view::politicians(ui, &self.politicians, &mut self.pol_search))
            }
            PaneKind::Alerts => {
                let (a, seen) = self.alerts_pane(ui);
                actions.extend(a);
                if seen {
                    self.worker.send(Request::MarkAlertsSeen);
                }
            }
            PaneKind::Wei => actions.extend(markets_view::wei(ui, &self.quotes, self.wei_updated.as_deref())),
            PaneKind::Curve => actions.extend(markets_view::curve(ui, self.curves.as_deref())),
            PaneKind::Search { query } => {
                let hits = if self.net_hits.0 == *query { self.net_hits.1.as_slice() } else { &[] };
                let (a, changed) = markets_view::search(ui, query, hits);
                actions.extend(a);
                if changed && query.trim().len() >= 2 {
                    self.search_pending = Some((query.clone(), Instant::now()));
                }
            }
            PaneKind::Research(r) => {
                let (a, refresh) = r.show(ui, &self.worker);
                actions.extend(a);
                if refresh {
                    r.request(id, &self.worker);
                }
            }
            PaneKind::News(n) => {
                let offline = self.provider.eq_ignore_ascii_case("sample");
                if let Some(refresh) = news(ui, n, offline, has_leaders) {
                    n.fetched = None;
                    n.loading = refresh;
                }
            }
            PaneKind::Options(o) => {
                let (a, refetch) = super::options_view::options_chain(ui, o);
                actions.extend(a);
                if refetch {
                    o.fetched = None;
                }
            }
            PaneKind::OrderBook(b) => {
                if let Some(a) = order_book(ui, b) {
                    actions.push(a);
                }
            }
            PaneKind::Portfolio(pp) => {
                let demo =
                    (self.prefs.trading.account_mode == crate::prefs::AccountMode::Demo).then_some(&*self.demo.pf);
                for a in super::portfolio_view::portfolio(ui, pp, &self.broker, demo, &self.quotes) {
                    use super::portfolio_view::PortAction as P;
                    match a {
                        P::Core(a) => actions.push(a),
                        P::Refresh => actions.push(Action::Command("PORT REFRESH".into())),
                        P::Connect => actions.push(Action::Command("PORT CONNECT".into())),
                        P::Setup => actions.push(Action::Command("PORT SETUP".into())),
                        P::Options(k) => actions.push(Action::Command(format!("{k} OMON"))),
                        P::News(k) => actions.push(Action::Command(format!("{k} CN"))),
                        P::Trade { key, buy, account, units } => self.open_ticket(
                            crate::command::TradeArgs {
                                buy: Some(buy),
                                symbol: Some(key),
                                units,
                                ..Default::default()
                            },
                            account,
                        ),
                        P::CancelOrder { account, id } => self.request_cancel(&account, &id),
                        P::ClosePosition { account, key } => self.close_position(&account, &key),
                        P::TradingSettings => {
                            self.settings_tab = super::settings_view::SettingsTab::Trading;
                            self.open_panel(PaneKind::Settings);
                        }
                        P::Open(url) => {
                            if let Err(e) = super::broker::open_external(url) {
                                self.status = format!("Could not open {url}: {e}");
                            }
                        }
                    }
                }
            }
            PaneKind::Watchlist
            | PaneKind::Settings
            | PaneKind::Help
            | PaneKind::Chart(_)
            | PaneKind::Desk(_)
            | PaneKind::Journal(_)
            | PaneKind::Stats(_)
            | PaneKind::Backtest(_)
            | PaneKind::Lookalike(_)
            | PaneKind::IntradayCompare(_)
            | PaneKind::SessionPath(_)
            | PaneKind::Breadth(_) => {}
        }
        if matches!(self.panes[i].kind, PaneKind::Desk(_)) {
            actions.extend(self.desk(ui, id));
        }
        if matches!(self.panes[i].kind, PaneKind::Journal(_)) {
            actions.extend(self.journal_pane(ui, id));
        }
        if matches!(self.panes[i].kind, PaneKind::Stats(_)) {
            actions.extend(self.stats_pane(ui, id));
        }
        if matches!(self.panes[i].kind, PaneKind::Backtest(_)) {
            actions.extend(self.backtest_pane(ui, id));
        }
        if matches!(self.panes[i].kind, PaneKind::Lookalike(_)) {
            actions.extend(self.lookalike_pane(ui, id));
        }
        if matches!(self.panes[i].kind, PaneKind::IntradayCompare(_)) {
            actions.extend(self.compare_pane(ui, id));
        }
        if matches!(self.panes[i].kind, PaneKind::SessionPath(_)) {
            actions.extend(self.session_path_pane(ui, id));
        }
        if matches!(self.panes[i].kind, PaneKind::Breadth(_)) {
            actions.extend(self.breadth_pane(ui, id));
        }
        match self.panes[i].kind {
            PaneKind::Watchlist => actions.extend(self.watchlist(ui)),
            PaneKind::Help => self.help_panel(ui),
            PaneKind::Settings => {
                let cmds = self.settings_panel(ui);
                self.run_settings_cmds(cmds);
            }
            _ => {}
        }
        (actions, false)
    }
}

/// Headlines list. Returns `Some(_)` when a refresh was requested.
fn news(ui: &mut Ui, n: &mut super::NewsPane, offline: bool, has_leaders: bool) -> Option<bool> {
    use tr_data::news::{NewsScope, format_freshness, format_pub};
    let mut refresh = None;
    let set_scope = |n: &mut super::NewsPane, scope: NewsScope, refresh: &mut Option<bool>| {
        if n.scope != scope {
            n.scope = scope;
            n.batch = None;
            *refresh = Some(true);
        }
    };
    ui.horizontal_wrapped(|ui| {
        let title = match n.scope {
            NewsScope::Symbol => n
                .symbol
                .as_deref()
                .map(|s| format!("{} headlines", tr_data::security(s).key()))
                .unwrap_or_else(|| "Headlines".into()),
            NewsScope::Watchlist => "Watchlist headlines".into(),
            NewsScope::Leaders => "Scanner-leader headlines".into(),
            NewsScope::Top => "Top market headlines".into(),
        };
        ui.label(RichText::new(title).font(ui_font(12.5)).color(text()));
        if n.loading {
            ui.add(egui::Spinner::new().size(12.0));
        }
        if ui
            .small_button("Watchlist")
            .on_hover_text("Deduplicated headlines for the selected watchlist group")
            .clicked()
        {
            set_scope(n, NewsScope::Watchlist, &mut refresh);
        }
        if has_leaders
            && ui
                .small_button("Leaders")
                .on_hover_text("Top names from the open SCAN pane — not the whole archive")
                .clicked()
        {
            set_scope(n, NewsScope::Leaders, &mut refresh);
        }
        if n.symbol.is_some()
            && ui.small_button("This name").on_hover_text("Headlines for the linked security").clicked()
        {
            set_scope(n, NewsScope::Symbol, &mut refresh);
        }
        if ui.small_button("Top News").on_hover_text("One general market query").clicked() {
            set_scope(n, NewsScope::Top, &mut refresh);
        }
        if ui.small_button("Refresh").clicked() {
            refresh = Some(true);
        }
    });
    ui.add_space(2.0);
    let now = chrono::Utc::now().timestamp();
    if let Some(batch) = &n.batch {
        let mut meta = vec![format_freshness(batch.fetched_at, now)];
        if !batch.queried.is_empty() {
            meta.push(format!("{} name{}", batch.queried.len(), if batch.queried.len() == 1 { "" } else { "s" }));
        }
        meta.push(format!("{} headline{}", batch.items.len(), if batch.items.len() == 1 { "" } else { "s" }));
        ui.label(RichText::new(meta.join(" · ")).font(ui_font(11.0)).color(muted()));
        if batch.sample || offline {
            ui.label(
                RichText::new("Sample / offline mode: headlines are not invented and never enter research.")
                    .font(ui_font(12.0))
                    .color(theme::down()),
            );
        }
        if batch.is_stale_at(now) {
            ui.label(
                RichText::new(format!(
                    "Stale feed — last successful fetch {}.",
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
                    batch.queried.len()
                ))
                .font(ui_font(12.0))
                .color(amber()),
            );
        } else if batch.items.is_empty() && !batch.failures.is_empty() {
            ui.label(
                RichText::new(format!("Could not load news: {}", batch.failures[0].error))
                    .font(ui_font(12.0))
                    .color(theme::down()),
            );
        }
    }
    let items = n.batch.as_ref().map(|b| b.items.as_slice()).unwrap_or(&[]);
    if items.is_empty() && !n.loading {
        let msg = if offline || n.batch.as_ref().is_some_and(|b| b.sample) {
            "No headlines in offline sample mode (TR_PROVIDER=sample)."
        } else if n.scope == NewsScope::Watchlist {
            "No headlines for this watchlist. Add names with WL ADD, or Refresh."
        } else if n.scope == NewsScope::Leaders {
            "No scanner leaders yet. Run SCAN, then open Leaders."
        } else {
            "No headlines for this security right now."
        };
        ui.label(RichText::new(msg).font(ui_font(12.0)).color(muted()));
    }
    let show_ticker = matches!(n.scope, NewsScope::Watchlist | NewsScope::Leaders);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let w = ui.available_width();
        for item in items {
            let when = format_pub(item.ts, now);
            let extra = if show_ticker { item.tickers.first().cloned().unwrap_or_default() } else { String::new() };
            let left = if extra.is_empty() { 60.0 } else { 108.0 };
            let title = ui.painter().layout(item.title.clone(), ui_font(13.0), text(), (w - left - 10.0).max(80.0));
            let h = title.size().y + dp(20.0);
            let (r, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
            widgets::hover_wash(ui, r, &resp, false);
            let p = ui.painter();
            p.text(pos2(r.left() + 6.0, r.top() + 4.0), Align2::LEFT_TOP, &when, mono(11.0), amber());
            if !extra.is_empty() {
                p.text(pos2(r.left() + 56.0, r.top() + 4.0), Align2::LEFT_TOP, &extra, mono(11.0), theme::up());
            }
            let tpos = pos2(r.left() + left, r.top() + 3.0);
            let th = title.size().y;
            p.galley(tpos, title, text());
            p.text(pos2(tpos.x, tpos.y + th + 2.0), Align2::LEFT_TOP, &item.publisher, ui_font(11.0), muted());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(&item.link));
            }
            resp.on_hover_text(&item.link).context_menu(|ui| {
                if ui.button("Open in Browser").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&item.link));
                    ui.close();
                }
                if ui.button("Copy Link").clicked() {
                    ui.ctx().copy_text(item.link.clone());
                    ui.close();
                }
            });
            p.hline(r.x_range(), r.bottom(), Stroke::new(1.0, theme::border()));
        }
    });
    refresh
}

/// Bucket sizes offered for a book around `mid`: raw plus three decades of the price scale.
pub fn group_steps(mid: f64) -> [f64; 4] {
    let base = 10f64.powi((mid.abs().max(1e-9).log10().floor() as i32) - 4);
    [0.0, base, base * 10.0, base * 100.0]
}

fn fmt_size(q: f64) -> String {
    if q >= 1e6 {
        format!("{:.2}M", q / 1e6)
    } else if q >= 1e3 {
        format!("{:.2}K", q / 1e3)
    } else if q >= 1.0 {
        format!("{q:.3}")
    } else {
        format!("{q:.5}")
    }
}

/// Price ladder: asks above, bids below, depth bars by cumulative size.
fn order_book(ui: &mut Ui, b: &mut super::BookPane) -> Option<Action> {
    let mut action = None;
    let key = tr_data::security(&b.symbol);
    let book: Option<OrderBook> = b.book.as_ref().map(|x| x.grouped(b.group));
    ui.horizontal_wrapped(|ui| {
        let r = ui.label(RichText::new(&key.ticker).font(mono(13.0)).color(amber())).interact(Sense::click());
        if r.clicked() {
            action = Some(Action::OpenChart(b.symbol.clone()));
        }
        r.on_hover_text("Open chart");
        if let Some(bk) = &book {
            ui.label(RichText::new(&bk.source).font(ui_font(11.5)).color(muted()));
            if bk.simulated {
                ui.label(
                    RichText::new(" SIMULATED ").font(ui_font(10.5)).color(Color32::BLACK).background_color(amber()),
                )
                .on_hover_text("No free public order book for this market (or offline); levels are synthetic.");
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        let depths = [10usize, 20, 50];
        let dl = ["10", "20", "50"];
        if let Some(i) =
            widgets::segmented(ui, ("book-depth", &b.symbol), &dl, depths.iter().position(|&x| x == b.depth), 20.0)
        {
            b.depth = depths[i];
            b.fetched = None;
        }
        let mid = b.book.as_ref().and_then(OrderBook::mid).unwrap_or(1.0);
        let steps = group_steps(mid);
        let d = key.decimals(mid);
        let labels: Vec<String> =
            steps.iter().map(|s| if *s == 0.0 { "Raw".into() } else { format!("{:.*}", d.min(8), s) }).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let sel = steps.iter().position(|s| (*s - b.group).abs() < 1e-12);
        if let Some(i) = widgets::segmented(ui, ("book-group", &b.symbol), &refs, sel, 20.0) {
            b.group = steps[i];
        }
    });
    if let Some(e) = &b.error {
        ui.label(RichText::new(e).font(ui_font(12.0)).color(theme::down()));
    }
    let Some(book) = book else {
        ui.add(egui::Spinner::new());
        return action;
    };
    let rect = ui.available_rect_before_wrap();
    let d = key.decimals(book.mid().unwrap_or(1.0));
    let row = dp(18.0);
    let rows_each = (((rect.height() - row * 2.0) / row / 2.0).floor() as usize).max(1);
    let asks: Vec<_> = book.asks.iter().take(rows_each).copied().collect();
    let bids: Vec<_> = book.bids.iter().take(rows_each).copied().collect();
    let cum = |v: &[(f64, f64)]| {
        v.iter()
            .scan(0.0, |acc, &(p, q)| {
                *acc += q;
                Some((p, q, *acc))
            })
            .collect::<Vec<_>>()
    };
    let (asks, bids) = (cum(&asks), cum(&bids));
    let max_cum = asks.last().map_or(0.0, |x| x.2).max(bids.last().map_or(0.0, |x| x.2)).max(1e-12);
    let p = ui.painter_at(rect);
    let w = rect.width();
    // Columns sized from the widest price so narrow panes stay legible.
    let price_w =
        p.layout_no_wrap(format!("{:.*}", d, book.asks.last().map_or(1.0, |l| l.0)), mono(12.0), text()).size().x;
    let show_total = w > price_w + 170.0;
    let x_price = rect.left() + (price_w + 8.0).max(w * 0.3);
    let x_total = rect.right() - 6.0;
    let x_size = if show_total { (x_price + x_total) / 2.0 } else { x_total };
    let mut y = rect.top();
    for (label, x) in [("Price", x_price), ("Size", x_size), ("Total", x_total)] {
        if label != "Total" || show_total {
            p.text(pos2(x, y + row / 2.0), Align2::RIGHT_CENTER, label, ui_font(11.0), muted());
        }
    }
    y += row;
    let draw = |y: f64, level: (f64, f64, f64), up: bool| {
        let y = y as f32;
        let c = if up { theme::up() } else { theme::down() };
        let frac = (level.2 / max_cum) as f32;
        let bar = Rect::from_min_max(pos2(rect.right() - w * frac, y + 1.0), pos2(rect.right(), y + row - 1.0));
        p.rect_filled(bar, 2.0, c.gamma_multiply(0.14));
        p.text(pos2(x_price, y + row / 2.0), Align2::RIGHT_CENTER, format!("{:.*}", d, level.0), mono(12.0), c);
        p.text(pos2(x_size, y + row / 2.0), Align2::RIGHT_CENTER, fmt_size(level.1), mono(12.0), text());
        if show_total {
            p.text(pos2(x_total, y + row / 2.0), Align2::RIGHT_CENTER, fmt_size(level.2), mono(12.0), muted());
        }
    };
    // Asks: best ask nearest the spread row.
    for (k, level) in asks.iter().enumerate().rev() {
        let at = y as f64 + (asks.len() - 1 - k) as f64 * row as f64;
        draw(at, *level, false);
    }
    y += asks.len() as f32 * row;
    let spread_r = Rect::from_min_size(pos2(rect.left(), y), vec2(w, row));
    p.rect_filled(spread_r, 3.0, theme::header());
    if let (Some(mid), Some(spread)) = (book.mid(), book.spread()) {
        p.text(pos2(x_price, spread_r.center().y), Align2::RIGHT_CENTER, format!("{:.*}", d, mid), mono(12.5), text());
        let spread_text = if show_total {
            format!("spread {:.*} ({:.1} bp)", d, spread, spread / mid * 1e4)
        } else {
            format!("{:.1} bp", spread / mid * 1e4)
        };
        p.text(pos2(x_total, spread_r.center().y), Align2::RIGHT_CENTER, spread_text, ui_font(11.0), muted());
    }
    y += row;
    for (k, level) in bids.iter().enumerate() {
        draw((y + k as f32 * row) as f64, *level, true);
    }
    ui.allocate_rect(rect, Sense::hover());
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_steps_scale_with_price() {
        assert_eq!(group_steps(82_000.0), [0.0, 1.0, 10.0, 100.0]);
        let fx = group_steps(1.09);
        assert!((fx[1] - 1e-4).abs() < 1e-12);
        assert_eq!(fmt_size(1_500.0), "1.50K");
        assert_eq!(fmt_size(0.5), "0.50000");
    }
}
