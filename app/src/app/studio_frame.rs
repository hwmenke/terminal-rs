//! Shared receipt and state chrome for newly touched Studio views.
use crate::{theme, widgets::MathCard};
use egui::Ui;
use tr_data::studio_dist::Report;
pub fn receipt(ui: &mut Ui, r: &Report, title: &str, summary: &str) {
    ui.heading(title);
    ui.label(format!(
        "{} · {} · CLOSED THRU {} · {} closed bars · {}",
        r.provenance.label(),
        r.timeframe.label(),
        super::studio_dist::stamp(r.end),
        r.bars,
        r.source
    ));
    ui.colored_label(theme::muted(), summary);
    if r.observed_at.is_none() {
        ui.colored_label(theme::caution(), "Observation clock unavailable; final bar withheld by loader");
    }
    super::studio_dist::gap_warning(ui, &r.gaps);
}
pub fn card(r: &Report, n: usize, id: &str, formula: &str) -> MathCard {
    MathCard::new(id, formula)
        .n(n, "closed horizon returns")
        .span(super::studio_dist::stamp(r.start), super::studio_dist::stamp(r.end))
        .closed_thru(super::studio_dist::stamp(r.end))
        .provenance(r.provenance)
        .source(&r.source)
}
/// Selection is ephemeral; clicking a tab never computes statistics.
pub fn tabs(ui: &mut Ui, selected: &mut usize, labels: &[&str]) {
    ui.horizontal_wrapped(|ui| {
        for (i, label) in labels.iter().enumerate() {
            ui.selectable_value(selected, i, *label);
        }
    });
}
pub fn evidence_mode(ui: &mut Ui, evidence: &mut bool) {
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(evidence, false, "Descriptive");
        ui.selectable_value(evidence, true, "Evidence");
        ui.colored_label(theme::muted(), "family: none · pointwise intervals, no hypothesis tests");
    });
}
pub fn low_n(ui: &mut Ui, n: usize, min: usize) {
    if n < min {
        let response = ui.colored_label(theme::caution(), format!("LOW N ({n} < {min}) · descriptive only"));
        let r = response.rect;
        ui.painter().line_segment(
            [r.right_top(), r.right_top() + egui::vec2(-5., 5.)],
            egui::Stroke::new(1., theme::caution()),
        );
    }
}
