//! Shared pane scaffolding: one header (`pane_header`), one state card (`analysis_state`),
//! a ticker chooser for "needs input" screens and a methodology-on-demand disclosure.
//!
//! Principles: instrument/period first, controls nearby, methodology on demand; loading,
//! empty, unavailable, stale and failed look different (label + tone + border), and a card
//! keeps a fixed minimum height so geometry does not jump when a result arrives.

use egui::{Align, Color32, Layout, Rect, RichText, Stroke, StrokeKind, Ui, pos2, vec2};

use super::{ChipPart, source_chip};
use crate::theme::{self, mono, ui_font};

/// What an analysis pane is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisKind {
    /// A request is in flight.
    Loading,
    /// A prerequisite (ticker, model, selection) is missing; the card offers the control.
    NeedsInput,
    /// The request succeeded and there is genuinely nothing to show (or filters exclude all).
    Empty,
    /// The source cannot supply this (unsupported instrument, no real history, optional service off).
    Unavailable,
    /// A previous snapshot is shown and may be out of date.
    Stale,
    /// The request failed; the reason is shown and a retry is offered by the caller.
    Failed,
    /// Results are shown.
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateBorder {
    None,
    Solid,
    Dashed,
}

/// How a state is drawn. Every variant differs in at least one of label, tone or border.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateLook {
    pub label: &'static str,
    pub tone: Color32,
    pub border: StateBorder,
}

impl AnalysisKind {
    #[cfg_attr(not(test), allow(dead_code))]
    pub const ALL: [Self; 7] =
        [Self::Loading, Self::NeedsInput, Self::Empty, Self::Unavailable, Self::Stale, Self::Failed, Self::Ready];
    pub fn look(self) -> StateLook {
        let (label, tone, border) = match self {
            // Blue is for active interaction: waiting on work and asking for input.
            Self::Loading => ("LOADING", theme::info(), StateBorder::Solid),
            Self::NeedsInput => ("CHOOSE", theme::accent(), StateBorder::Solid),
            // Grey: nothing there, or an optional/unsupported source. Never red/green (those mean direction).
            Self::Empty => ("NO RESULTS", theme::muted(), StateBorder::None),
            Self::Unavailable => ("UNAVAILABLE", theme::muted(), StateBorder::Dashed),
            Self::Stale => ("PREVIOUS SNAPSHOT", theme::caution(), StateBorder::Solid),
            Self::Failed => ("FAILED", theme::alert(), StateBorder::Solid),
            Self::Ready => ("READY", theme::text(), StateBorder::None),
        };
        StateLook { label, tone, border }
    }
}

/// A state plus its words. `retained` means a previous result is still on screen: the state is
/// then a slim banner (fixed height) and the caller keeps painting the old snapshot below it.
#[derive(Debug, Clone, Copy)]
pub struct AnalysisState<'a> {
    pub kind: AnalysisKind,
    pub title: &'a str,
    pub detail: &'a str,
    pub retained: bool,
}

impl<'a> AnalysisState<'a> {
    pub fn new(kind: AnalysisKind, title: &'a str, detail: &'a str) -> Self {
        Self { kind, title, detail, retained: false }
    }
    pub fn loading(title: &'a str) -> Self {
        Self::new(AnalysisKind::Loading, title, "")
    }
    pub fn needs_input(title: &'a str, detail: &'a str) -> Self {
        Self::new(AnalysisKind::NeedsInput, title, detail)
    }
    pub fn empty(title: &'a str, detail: &'a str) -> Self {
        Self::new(AnalysisKind::Empty, title, detail)
    }
    pub fn unavailable(title: &'a str, detail: &'a str) -> Self {
        Self::new(AnalysisKind::Unavailable, title, detail)
    }
    pub fn stale(title: &'a str, detail: &'a str) -> Self {
        Self::new(AnalysisKind::Stale, title, detail)
    }
    pub fn failed(title: &'a str, detail: &'a str) -> Self {
        Self::new(AnalysisKind::Failed, title, detail)
    }
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn ready() -> Self {
        Self::new(AnalysisKind::Ready, "", "")
    }
    /// Keep the previous snapshot visible under a slim banner.
    pub fn retaining(mut self) -> Self {
        self.retained = true;
        self
    }
    /// Whether the caller should paint result content under this state.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn shows_results(&self) -> bool {
        self.kind == AnalysisKind::Ready || (self.retained && matches!(self.kind, AnalysisKind::Loading | AnalysisKind::Stale))
    }
}

fn outline(ui: &Ui, rect: Rect, look: StateLook) {
    let p = ui.painter();
    match look.border {
        StateBorder::None => {}
        StateBorder::Solid => {
            p.rect_stroke(rect, theme::CARD_RADIUS, Stroke::new(1.0, look.tone.gamma_multiply(0.7)), StrokeKind::Inside);
        }
        StateBorder::Dashed => {
            let (a, b, c, d) = (rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom());
            let s = Stroke::new(1.0, look.tone.gamma_multiply(0.6));
            for pts in [vec![a, b], vec![b, c], vec![c, d], vec![d, a]] {
                p.extend(egui::Shape::dashed_line(&pts, s, 4.0, 3.0));
            }
        }
    }
}

fn label_chip(ui: &mut Ui, look: StateLook) {
    let g = ui.painter().layout_no_wrap(look.label.into(), mono(9.5), look.tone);
    let (r, _) = ui.allocate_exact_size(vec2(g.size().x + 10.0, 16.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 4.0, look.tone.gamma_multiply(0.14));
    ui.painter().galley(pos2(r.left() + 5.0, r.center().y - g.size().y / 2.0), g, look.tone);
}

/// Render a state. Returns `true` when the caller should paint its result content (Ready, or a
/// retained previous snapshot under a banner). For every other state this paints a card of at
/// least [`theme::STATE_CARD_H`] and calls `body` for the actionable control (picker, retry…).
pub fn analysis_state(ui: &mut Ui, state: &AnalysisState, body: impl FnOnce(&mut Ui)) -> bool {
    let look = state.kind.look();
    if state.kind == AnalysisKind::Ready {
        return true;
    }
    if state.retained && matches!(state.kind, AnalysisKind::Loading | AnalysisKind::Stale) {
        // Slim, fixed-height banner: the old numbers stay put and are labelled as such.
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), egui::Sense::hover());
        ui.painter().rect_filled(r, theme::CONTROL_RADIUS, look.tone.gamma_multiply(0.10));
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(theme::SP_S, 0.0))));
        c.horizontal_centered(|ui| {
            if state.kind == AnalysisKind::Loading {
                ui.add(egui::Spinner::new().size(11.0).color(look.tone));
            }
            let words = match (state.kind, state.title.is_empty()) {
                (AnalysisKind::Loading, _) => "Updating · showing previous snapshot".to_string(),
                (_, true) => "Previous snapshot".to_string(),
                (_, false) => format!("Previous snapshot · {}", state.title),
            };
            ui.label(RichText::new(words).font(ui_font(11.0)).color(look.tone));
            body(ui);
        });
        return true;
    }
    let inner = egui::Frame::new()
        .fill(theme::card())
        .corner_radius(theme::CARD_RADIUS)
        .inner_margin(theme::SP_XL)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(theme::STATE_CARD_H - 2.0 * theme::SP_XL);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.y = theme::SP_S;
                ui.horizontal(|ui| {
                    label_chip(ui, look);
                    if state.kind == AnalysisKind::Loading {
                        ui.add(egui::Spinner::new().size(12.0).color(look.tone));
                    }
                });
                if !state.title.is_empty() {
                    ui.label(RichText::new(state.title).font(ui_font(14.0)).strong().color(theme::text()));
                }
                if !state.detail.is_empty() {
                    ui.add(egui::Label::new(RichText::new(state.detail).font(ui_font(12.0)).color(theme::muted())).wrap());
                }
                ui.add_space(theme::SP_XS);
                body(ui);
            });
        });
    outline(ui, inner.response.rect, look);
    ui.add_space(theme::SP_L);
    false
}

/// Where a pane's data came from, shown as one muted line under the title row.
pub struct HeaderSource<'a> {
    pub lead: &'a str,
    pub parts: &'a [ChipPart<'a>],
    /// Used (truncated) only when no structured receipt exists; empty is fine.
    pub legacy: &'a str,
}

/// Standard pane header: `title · context` on the left, actions on the right, an optional
/// one-line source chip below. Fixed row height so panes line up.
pub fn pane_header(
    ui: &mut Ui,
    title: &str,
    context: &str,
    source: Option<HeaderSource>,
    actions: impl FnOnce(&mut Ui),
) {
    ui.allocate_ui_with_layout(vec2(ui.available_width(), theme::CONTROL_H), Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = theme::SP_M;
        ui.label(RichText::new(title).font(ui_font(14.0)).strong().color(theme::text()));
        if !context.is_empty() {
            ui.label(RichText::new(context).font(mono(11.0)).color(theme::muted()));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = theme::SP_S;
            actions(ui);
        });
    });
    if let Some(src) = source {
        source_chip(ui, src.lead, src.parts, || src.legacy.to_string());
    }
}

/// A header action with the shared control height. Blue only when it is the active choice.
pub fn header_button(ui: &mut Ui, label: &str, hover: &str, active: bool) -> egui::Response {
    let text = RichText::new(label).font(ui_font(12.0)).color(if active { theme::accent() } else { theme::text() });
    ui.add(egui::Button::new(text).min_size(vec2(0.0, theme::CONTROL_H - 4.0))).on_hover_text(hover)
}

/// Methodology on demand: one collapsed line, the explanation only when asked for.
pub fn methodology(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, body: &str) {
    egui::CollapsingHeader::new(RichText::new("Method").font(ui_font(11.0)).color(theme::muted()))
        .id_salt(id)
        .default_open(false)
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(body).font(ui_font(11.5)).color(theme::muted())).wrap());
        });
}

/// Ticker chooser for a "needs input" state: a text field, one confirm button and up to six
/// watchlist shortcuts. Returns the normalised security key once the user commits one.
/// `buf` is edited in place; nothing is requested until the user acts.
pub fn ticker_picker(ui: &mut Ui, buf: &mut String, suggestions: &[String], cta: &str) -> Option<String> {
    let mut chosen = None;
    ui.horizontal(|ui| {
        let edit = ui.add_sized(
            [140.0, theme::CONTROL_H - 4.0],
            egui::TextEdit::singleline(buf).hint_text("AAPL").font(mono(12.0)),
        );
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let go = ui
            .add_enabled(
                !buf.trim().is_empty(),
                egui::Button::new(RichText::new(cta).font(ui_font(12.0)).color(Color32::WHITE))
                    .fill(theme::accent())
                    .min_size(vec2(0.0, theme::CONTROL_H - 4.0)),
            )
            .on_disabled_hover_text("Type a ticker first")
            .clicked();
        if (enter || go) && !buf.trim().is_empty() {
            chosen = Some(tr_data::security(buf.trim()).key());
        }
    });
    if !suggestions.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Watchlist").font(ui_font(11.0)).color(theme::muted()));
            for key in suggestions.iter().take(6) {
                let t = tr_data::security(key).ticker;
                if ui
                    .add(egui::Button::new(RichText::new(&t).font(mono(11.0))).min_size(vec2(0.0, theme::CONTROL_H - 6.0)))
                    .on_hover_text(format!("Use {t}"))
                    .clicked()
                {
                    chosen = Some(key.clone());
                }
            }
        });
    }
    chosen
}

/// "Choose a ticker" setup state for a screen opened without a focused instrument: a neutral
/// (blue-family, never red) card with the picker as its action. Returns the chosen key.
pub fn needs_ticker(ui: &mut Ui, what: &str, buf: &mut String, watchlist: &[String]) -> Option<String> {
    let mut chosen = None;
    analysis_state(ui, &AnalysisState::needs_input("Choose a ticker", what), |ui| {
        chosen = ticker_picker(ui, buf, watchlist, "Use ticker");
    });
    chosen
}

/// Whether two ticker spellings name the same security key.
pub fn same_security(a: &str, b: &str) -> bool {
    tr_data::security(a.trim()).key() == tr_data::security(b.trim()).key()
}

/// The ticker field of a loaded analysis. It edits `draft`, never the committed symbol, so
/// emptying it mid-edit cannot flip the pane back to the chooser. Returns `true` on Enter.
/// While the field is not focused the draft always mirrors `symbol` (a blur discards an
/// uncommitted edit, and link-driven symbol changes show up), so committing the draft is a
/// no-op unless the user pressed Enter in the field.
pub fn anchor_field(ui: &mut Ui, label: &str, draft: &mut String, symbol: &str) -> bool {
    ui.label(RichText::new(label).color(theme::muted()));
    let id = ui.id().with(("anchor-field", label));
    if !ui.memory(|m| m.has_focus(id)) {
        *draft = symbol.to_string();
    }
    let edit = ui.add(egui::TextEdit::singleline(draft).id(id).desired_width(140.0).hint_text("AAPL"));
    edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// Write the draft into the committed symbol (non-empty drafts only), as a normalised key.
pub fn commit_anchor(symbol: &mut String, draft: &mut String) {
    if !draft.trim().is_empty() {
        *symbol = tr_data::security(draft.trim()).key();
    }
    draft.clear();
}

/// A typo in the request (bar size, sessions…) is an input mistake, not a failed run: a neutral
/// "check the inputs" card (blue-family, never red) with the parser's own words.
pub fn input_error_card(ui: &mut Ui, detail: &str) {
    analysis_state(ui, &AnalysisState::needs_input("Check the inputs", detail), |_| {});
}

/// Whether an error message says the data cannot be supplied (refused, unverified, unavailable)
/// as opposed to a run that failed.
pub fn error_is_unavailability(e: &str) -> bool {
    let e = e.to_ascii_lowercase();
    ["refused", "unavailable", "not available", "no verified", "verified real data", "unsupported"]
        .iter()
        .any(|k| e.contains(k))
}

/// The standard loading / failed / empty / ready ladder of a single-result pane. Returns `true`
/// when the caller should paint its result. `retry` is set when the user presses the card's
/// action. A previous result stays visible (labelled as such) while a new one loads.
pub fn run_state(
    ui: &mut Ui,
    loading: bool,
    error: Option<&str>,
    has_result: bool,
    loading_title: &str,
    retry: &mut bool,
) -> bool {
    let action = |label: &'static str| {
        move |ui: &mut Ui| {
            if ui.add(egui::Button::new(label).min_size(vec2(0.0, theme::CONTROL_H - 4.0))).clicked() {
                return true;
            }
            false
        }
    };
    let mut pressed = false;
    let show = if loading {
        let s = AnalysisState::loading(loading_title);
        analysis_state(ui, &if has_result { s.retaining() } else { s }, |_| {})
    } else if let (Some(e), true) = (error, has_result) {
        // The refresh failed but an earlier result is still on screen: keep it, labelled.
        let run = action("Retry");
        let head: String = e.chars().take(80).collect();
        let title = format!("refresh failed: {head}");
        analysis_state(ui, &AnalysisState::stale(&title, e).retaining(), |ui| pressed = run(ui))
    } else if let Some(e) = error {
        let run = action("Retry");
        // A source that cannot supply real data is "unavailable" (grey, dashed), not a failure.
        let state = if error_is_unavailability(e) {
            AnalysisState::unavailable("No verified data for this request", e)
        } else {
            AnalysisState::failed("Could not complete this analysis", e)
        };
        analysis_state(ui, &state, |ui| pressed = run(ui))
    } else if has_result {
        true
    } else {
        let run = action("Run");
        analysis_state(ui, &AnalysisState::empty("Nothing to show yet", "Run the analysis to load verified data."), |ui| {
            pressed = run(ui)
        })
    };
    *retry |= pressed;
    show
}

/// Test support: every painted text with its effective colour, in paint order.
#[cfg(test)]
pub(crate) fn painted_texts(out: &egui::FullOutput) -> Vec<(String, Color32)> {
    fn walk(s: &egui::Shape, acc: &mut Vec<(String, Color32)>) {
        match s {
            egui::Shape::Text(t) => {
                let c = t.galley.job.sections.first().map(|s| s.format.color).unwrap_or(Color32::PLACEHOLDER);
                let c = if c == Color32::PLACEHOLDER { t.override_text_color.unwrap_or(t.fallback_color) } else { c };
                acc.push((t.galley.job.text.clone(), c));
            }
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, acc)),
            _ => {}
        }
    }
    let mut acc = Vec::new();
    out.shapes.iter().for_each(|c| walk(&c.shape, &mut acc));
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_looks_different() {
        let looks: Vec<_> = AnalysisKind::ALL.iter().map(|k| k.look()).collect();
        for (i, a) in looks.iter().enumerate() {
            for b in &looks[i + 1..] {
                assert_ne!(a, b);
                assert_ne!(a.label, b.label, "labels must differ so states read apart without colour");
            }
        }
        // Failure is the only alert-toned state; stale is amber; waiting/asking is blue-family.
        assert_eq!(AnalysisKind::Failed.look().tone, theme::alert());
        assert_eq!(AnalysisKind::NeedsInput.look().tone, theme::accent());
        assert_ne!(AnalysisKind::Unavailable.look().border, AnalysisKind::Empty.look().border);
    }

    #[test]
    fn refused_and_unavailable_errors_are_not_failures() {
        assert!(error_is_unavailability("Intraday compare refused: Missing. Verified real data is required."));
        assert!(error_is_unavailability("Options unavailable for SPX"));
        assert!(!error_is_unavailability("request timed out"));
    }

    #[test]
    fn run_state_ladder_picks_distinct_cards() {
        let ctx = egui::Context::default();
        let run = |loading, err: Option<&str>, has| {
            let mut retry = false;
            let mut shown = false;
            let mut out = ctx.run_ui(Default::default(), |ui| {
                shown = run_state(ui, loading, err, has, "Reading…", &mut retry);
            });
            out.textures_delta.clear();
            (shown, painted_texts(&out))
        };
        let has = |t: &[(String, Color32)], s: &str| t.iter().any(|(x, _)| x == s);
        let (shown, t) = run(true, None, false);
        assert!(!shown && has(&t, "LOADING"));
        let (shown, t) = run(true, None, true);
        assert!(shown && has(&t, "Updating · showing previous snapshot"));
        let (shown, t) = run(false, Some("request timed out"), true);
        assert!(shown && t.iter().any(|(x, _)| x.starts_with("Previous snapshot · refresh failed")));
        let (shown, t) = run(false, Some("request timed out"), false);
        assert!(!shown && has(&t, "FAILED") && has(&t, "Retry"));
        let (_, t) = run(false, Some("refused: Verified real data is required"), false);
        assert!(has(&t, "UNAVAILABLE") && !has(&t, "FAILED"));
        let (shown, t) = run(false, None, false);
        assert!(!shown && has(&t, "NO RESULTS") && has(&t, "Run"));
        let (shown, t) = run(false, None, true);
        assert!(shown && t.is_empty());
    }

    #[test]
    fn needs_input_is_never_red_or_green() {
        let t = AnalysisKind::NeedsInput.look().tone;
        assert!(t != theme::down() && t != theme::up() && t != theme::alert());
    }

    #[test]
    fn retained_loading_keeps_results_and_fixed_banner_height() {
        let ctx = egui::Context::default();
        let heights: Vec<f32> = [AnalysisState::loading("").retaining(), AnalysisState::stale("older", "").retaining()]
            .iter()
            .map(|s| {
                let mut h = 0.0;
                ctx.run_ui(Default::default(), |ui| {
                    let top = ui.cursor().top();
                    assert!(analysis_state(ui, s, |_| {}));
                    h = ui.cursor().top() - top;
                })
                .textures_delta
                .clear();
                h
            })
            .collect();
        assert!(heights.iter().all(|h| (*h - heights[0]).abs() < 0.5 && *h < 40.0), "{heights:?}");
    }

    #[test]
    fn cards_keep_min_height_and_only_ready_or_retained_show_results() {
        let ctx = egui::Context::default();
        for kind in AnalysisKind::ALL {
            let s = AnalysisState::new(kind, "t", "d");
            let mut h = 0.0;
            let mut shows = false;
            ctx.run_ui(Default::default(), |ui| {
                let top = ui.cursor().top();
                shows = analysis_state(ui, &s, |_| {});
                h = ui.cursor().top() - top;
            })
            .textures_delta
            .clear();
            assert_eq!(shows, kind == AnalysisKind::Ready, "{kind:?}");
            if kind != AnalysisKind::Ready {
                assert!(h >= theme::STATE_CARD_H - 0.5, "{kind:?} {h}");
            }
        }
        assert!(AnalysisState::loading("x").retaining().shows_results());
        assert!(!AnalysisState::failed("x", "y").retaining().shows_results());
    }
}
