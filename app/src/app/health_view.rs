//! Feed diagnostics and shared retry routing. This pane never fetches on paint.
use super::{PaneKind, TerminalApp};
use crate::worker::Request;
use crate::{theme, widgets};
use egui::{Align2, Color32, Rect, Sense, Stroke, Ui, pos2, vec2};
use tr_data::health::{DataMode, Feed, FeedState, Outcome};

pub(super) fn auto_retry_ready(state: &FeedState) -> bool {
    !matches!(state, FeedState::Unsupported { .. })
        && state.retry_at().is_none_or(|at| at <= chrono::Utc::now().timestamp())
}
fn color(state: &FeedState) -> Color32 {
    match state {
        FeedState::Live { .. } => theme::up(),
        FeedState::Stale { .. } => theme::amber(),
        FeedState::Unavailable(_) => Color32::from_rgb(225, 74, 83),
        _ => theme::muted(),
    }
}
fn label(state: &FeedState) -> &'static str {
    match state {
        FeedState::Live { .. } => "LIVE",
        FeedState::Stale { .. } => "STALE",
        FeedState::Unavailable(_) => "UNAVAILABLE",
        FeedState::Unsupported { .. } => "UNSUPPORTED",
        FeedState::Unknown => "UNKNOWN",
    }
}
impl TerminalApp {
    pub(super) fn on_feed(
        &mut self,
        feed: Feed,
        pane: Option<u64>,
        symbol: Option<String>,
        generation: Option<u64>,
        state: FeedState,
    ) {
        let now = chrono::Utc::now().timestamp();
        if let Some(id) = pane {
            let Some(p) = self.panes.iter_mut().find(|p| p.id == id) else { return };
            if symbol.as_deref().is_some_and(|s| p.kind.symbol() != Some(s)) {
                return;
            }
            match &mut p.kind {
                PaneKind::Chart(c) if feed == Feed::Bars => c.on_feed(generation, state.clone()),
                PaneKind::OrderBook(b) if feed == Feed::Book => {
                    b.feed_state = if b.book.as_ref().is_some_and(|b| !b.simulated) {
                        state.stale(b.last_ok.unwrap_or(now), &b.book.as_ref().unwrap().source, now)
                    } else {
                        state.clone()
                    };
                    if b.feed_state.is_down() {
                        b.book = None;
                    }
                }
                PaneKind::Options(o) if feed == Feed::Options => {
                    o.feed_state = if let Some(c) = o.chain.as_ref().filter(|c| !c.simulated) {
                        state.stale(c.ts, &c.source, now)
                    } else {
                        state.clone()
                    };
                    if o.feed_state.is_down() {
                        o.chain = None;
                        o.call_greeks.clear();
                        o.put_greeks.clear();
                    }
                }
                _ => {}
            }
        } else if feed == Feed::Quotes {
            if let Some(key) = symbol {
                let shown = self
                    .quotes
                    .get(&key)
                    .filter(|q| q.provenance.is_verified())
                    .map(|q| state.stale(q.ts, &self.provider, now))
                    .unwrap_or_else(|| state.clone());
                self.quote_states.insert(key, shown);
            }
        } else if feed == Feed::Curve {
            self.curve_state = self
                .curves
                .as_ref()
                .map(|c| state.stale(c.current.ts, &self.provider, now))
                .unwrap_or_else(|| state.clone());
        }
    }
    pub(super) fn display_health(&self, feed: Feed) -> FeedState {
        let state = &self.health[feed as usize].state;
        if !state.is_down() {
            return state.clone();
        }
        for p in &self.panes {
            let state = match (&p.kind, feed) {
                (PaneKind::Chart(c), Feed::Bars) => c.feed_display(),
                (PaneKind::OrderBook(b), Feed::Book) => b.feed_state.clone(),
                (PaneKind::Options(o), Feed::Options) => o.feed_state.clone(),
                _ => continue,
            };
            if matches!(state, FeedState::Stale { .. }) {
                return state;
            }
        }
        if feed == Feed::Curve && matches!(self.curve_state, FeedState::Stale { .. }) {
            return self.curve_state.clone();
        }
        if feed == Feed::Quotes
            && let Some(stale) = self.quote_states.values().find(|s| matches!(s, FeedState::Stale { .. }))
        {
            return stale.clone();
        }
        state.clone()
    }
    pub(super) fn retry_focused(&self) -> bool {
        let Some(p) = self.panes.iter().find(|p| p.id == self.focus) else { return false };
        let (feed, pane, available) = match &p.kind {
            PaneKind::Health => (self.health_selected, None, true),
            PaneKind::Chart(c) if !c.archive => {
                (Feed::Bars, Some(p.id), c.feed_state.is_down() || matches!(c.feed_state, FeedState::Stale { .. }))
            }
            PaneKind::OrderBook(b) => {
                (Feed::Book, Some(p.id), !matches!(b.feed_state, FeedState::Live { .. } | FeedState::Unknown))
            }
            PaneKind::Options(o) => {
                (Feed::Options, Some(p.id), !matches!(o.feed_state, FeedState::Live { .. } | FeedState::Unknown))
            }
            PaneKind::Curve => {
                (Feed::Curve, None, !matches!(self.curve_state, FeedState::Live { .. } | FeedState::Unknown))
            }
            PaneKind::Wei | PaneKind::Watchlist => (
                Feed::Quotes,
                None,
                self.quote_states.values().any(|s| !matches!(s, FeedState::Live { .. } | FeedState::Unknown)),
            ),
            _ => return false,
        };
        if available {
            self.worker.send(Request::RetryFeed { feed, pane });
        }
        available
    }
    pub(super) fn health_pane(&mut self, ui: &mut Ui) {
        ui.label(
            egui::RichText::new(if self.data_mode == DataMode::ExplicitSample {
                "EXPLICIT SAMPLE (TR_PROVIDER=sample)".into()
            } else {
                format!("LIVE · {}", self.provider)
            })
            .font(theme::mono(13.0))
            .color(theme::text()),
        );
        ui.label(
            egui::RichText::new("Select a feed · R retry · observations are per request")
                .font(theme::mono(10.0))
                .color(theme::muted()),
        );
        egui::ScrollArea::vertical().show(ui, |ui| {
            let now = chrono::Utc::now().timestamp();
            for feed in Feed::ALL {
                let h = &self.health[feed as usize];
                let state = self.display_health(feed);
                let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 96.0), Sense::click());
                if response.clicked() {
                    self.health_selected = feed;
                }
                let p = ui.painter_at(rect);
                widgets::data_card(
                    &p,
                    rect,
                    &format!("{} · {}", feed.label(), h.source),
                    color(&state),
                    self.health_selected == feed,
                );
                widgets::card_footer(&p, rect, "");
                let age = h.last_ok.map(|at| widgets::age_label(now - at)).unwrap_or_else(|| "never".into());
                let retry = state.retry_at().map(|t| format!(" · retry in {}s", (t - now).max(0))).unwrap_or_default();
                let mut pill = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                    pos2((rect.right() - 112.0).max(rect.left()), rect.top() + 6.0),
                    vec2(108.0, 16.0),
                )));
                let kind = match state {
                    FeedState::Unavailable(_) => widgets::ChipKind::Unavailable,
                    FeedState::Stale { .. } => widgets::ChipKind::Caution,
                    _ => widgets::ChipKind::Neutral,
                };
                widgets::chip(&mut pill, kind, label(&state));
                p.text(
                    rect.min + vec2(8.0, 33.0),
                    Align2::LEFT_CENTER,
                    format!("last ok {age} · ok {} / fail {}{retry}", h.ok, h.failed),
                    theme::mono(10.0),
                    theme::muted(),
                );
                let error = h.last_err.as_deref().unwrap_or("No errors recorded");
                let galley = p.layout(error.into(), theme::mono(10.0), theme::muted(), (rect.width() - 16.0).max(1.0));
                p.galley(rect.min + vec2(8.0, 43.0), galley, theme::muted());
                let width = ((rect.width() - 76.0).max(60.0) / 60.0).min(8.0);
                for (i, outcome) in h.recent.iter().enumerate() {
                    let r = Rect::from_min_size(
                        pos2(rect.left() + 8.0 + i as f32 * width, rect.bottom() - 12.0),
                        vec2((width - 1.0).max(1.0), 5.0),
                    );
                    match outcome {
                        Outcome::Ok => {
                            p.rect_filled(r, 0.0, theme::up());
                        }
                        Outcome::Failed => {
                            p.rect_filled(r, 0.0, Color32::from_rgb(225, 74, 83));
                        }
                        Outcome::None => {
                            p.hline(r.x_range(), r.center().y, Stroke::new(1.0, theme::border()));
                        }
                    }
                }
                p.text(
                    rect.right_bottom() + vec2(-8.0, -12.0),
                    Align2::RIGHT_CENTER,
                    "R retry",
                    theme::mono(9.0),
                    theme::muted(),
                );
                response.on_hover_text(error);
                if feed == Feed::Bars && ui.small_button("VAULT › · per-symbol session coverage").clicked() {
                    self.vault_command(crate::command::VaultCmd::Open, self.focused_symbol());
                }
                ui.add_space(6.0);
            }
        });
        if ui.button("Retry selected feed (R)").clicked() {
            self.worker.send(Request::RetryFeed { feed: self.health_selected, pane: None });
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    }
    pub(super) fn health_strip(&mut self, ui: &Ui, rect: Rect) {
        let p = ui.painter_at(rect);
        if self.data_mode == DataMode::ExplicitSample || self.provider == "offline" {
            let (text, color) = if self.data_mode == DataMode::ExplicitSample {
                ("SAMPLE · SYNTHETIC", theme::amber())
            } else {
                ("OFFLINE · NO DATA SOURCE", Color32::from_rgb(225, 74, 83))
            };
            p.rect_filled(rect.shrink2(vec2(3.0, 3.0)), 5.0, color);
            p.text(rect.center(), Align2::CENTER_CENTER, text, theme::mono(10.0), Color32::BLACK);
            if ui.interact(rect, ui.id().with("feed-health"), Sense::click()).clicked() {
                self.open_panel(PaneKind::Health);
            }
            return;
        }
        let width = rect.width() / 6.0;
        for (i, feed) in Feed::ALL.into_iter().enumerate() {
            let state = self.display_health(feed);
            let h = &self.health[i];
            let r = Rect::from_min_size(rect.min + vec2(i as f32 * width, 0.0), vec2(width, rect.height()));
            let center = pos2(r.left() + 6.0, r.center().y);
            let color = color(&state);
            if matches!(state, FeedState::Unknown | FeedState::Unsupported { .. }) {
                p.circle_stroke(center, 3.0, Stroke::new(1.0, color));
            } else {
                let alpha = if feed == Feed::Ticks && matches!(state, FeedState::Live { .. }) {
                    ui.input(|i| (i.time * 3.0).sin() as f32 * 0.2 + 0.8)
                } else {
                    1.0
                };
                p.circle_filled(center, 3.0, color.gamma_multiply(alpha));
            }
            p.text(center + vec2(6.0, 0.0), Align2::LEFT_CENTER, feed.label(), theme::mono(9.0), color);
            let now = chrono::Utc::now().timestamp();
            let tooltip = format!(
                "{} · {}\nlast ok {}\n{}\n{}",
                h.source,
                label(&state),
                h.last_ok.map(|t| format!("{} ago", widgets::age_label(now - t))).unwrap_or_else(|| "never".into()),
                h.last_err.as_deref().unwrap_or("No errors"),
                state.retry_at().map(|t| format!("retry in {}s", (t - now).max(0))).unwrap_or_default()
            );
            if ui.interact(r, ui.id().with(("feed-health", i)), Sense::click()).on_hover_text(tooltip).clicked() {
                self.open_panel(PaneKind::Health);
            }
        }
    }
}
