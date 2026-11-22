//! Dedicated When lens: rule editor, fixed-family evidence rows, shared-bin
//! distributions, probability tiles and prior-only buckets. Statistics, cards
//! and mask runs are prepared on workers; this module paints immutable Arcs.
use crate::{
    theme,
    widgets::{MathCard, MathCardExt},
    worker::{Request, Worker, studio_token},
};
use egui::{Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use std::sync::{Arc, LazyLock};
use tr_core::Timeframe;
use tr_data::{
    report_cache::{ReportCache, ReportKey},
    studio_when::{self as data, SharedDistribution, WhenBootstrap, WhenReport},
};

static REPORTS: LazyLock<ReportCache> = LazyLock::new(|| ReportCache::new(4));
static BOOTSTRAPS: LazyLock<ReportCache> = LazyLock::new(|| ReportCache::new(8));
static SUBSETS: LazyLock<ReportCache> = LazyLock::new(|| ReportCache::new(4));

/// Final geometry reserved while loading so the lens does not jump.
pub const BODY_HEIGHT: f32 = 430.;

pub struct PreparedWhen {
    pub report: WhenReport,
    /// Per horizon: [mean contrast, hit contrast].
    pub cards: Vec<[MathCard; 2]>,
    pub probability_cards: Vec<MathCard>,
    pub bucket_cards: Vec<MathCard>,
    pub distribution_cards: Vec<MathCard>,
    /// Contiguous accepted rule slots `[start, end)` for canvas shading.
    pub runs: Vec<(i64, i64)>,
    pub receipt: String,
    /// Structured source receipt of the analysed series (chip + Inspector); not part of the key.
    pub source_receipts: Vec<tr_data::source_receipt::SourceReceipt>,
    /// Rule text exactly as requested (for response matching).
    pub requested_rule: String,
    /// Printed canvas label for the accepted mask.
    pub shading_label: String,
}

pub struct PreparedSubset {
    pub report: data::SubsetReport,
    pub card: MathCard,
    pub label: String,
}

fn stamp(t: i64) -> String {
    super::studio_dist::stamp(t)
}
fn signed(x: f64) -> String {
    format!("{x:+.2}").replace('-', "−")
}
fn opt(x: Option<f64>, digits: usize) -> String {
    x.map(|v| format!("{v:.digits$}")).unwrap_or_else(|| "unavailable".into())
}
fn base(receipt: &tr_data::studio::SeriesReceipt, ts: &[i64], id: &str, formula: &str) -> MathCard {
    MathCard::new(id, formula)
        .source(&receipt.source)
        .provenance(receipt.provenance)
        .span(
            ts.first().map_or("unavailable".into(), |t| stamp(*t)),
            ts.last().map_or("unavailable".into(), |t| stamp(*t)),
        )
        .closed_thru(stamp(receipt.closed_thru))
        .input(
            "observation clock",
            receipt.observed_at.map(stamp).unwrap_or_else(|| "unknown; forming bar withheld".into()),
        )
}

/// Worker-side preparation of every card and the shading runs.
pub fn prepare(report: WhenReport, requested_rule: String) -> PreparedWhen {
    let r = &report;
    let receipt = &r.mask.receipt;
    let ts = &r.mask.ts;
    let cards = r
        .cells
        .iter()
        .map(|c| {
            std::array::from_fn(|j| {
                let (e, q, id, y) = if j == 0 {
                    let id = "when.contrast.mean · WHEN MEAN CONTRAST";
                    (&c.mean_contrast, c.q_mean, id, "simple forward return (bp)")
                } else {
                    (&c.hit_contrast, c.q_hit, "when.contrast.hit · WHEN EVENT CONTRAST", "I(R > X), fraction")
                };
                let mut card = base(
                    receipt,
                    ts,
                    id,
                    "R[t,h]=10⁴(C[t+h]/C[t]−1); g̃=g−ḡ_c, Ỹ=Y−Ȳ_c; τ=Σg̃Ỹ/D, D=Σg̃²; s[t]=g̃[t]e[t]/D; Var=Σs²+2Σ_l(1−l/(L+1))Σ_t s[t]s[t−l]",
                )
                .value(e.estimate.map(signed).unwrap_or_else(|| "unavailable".into()))
                .n(c.raw_n, "completed outcomes")
                .input("Y / horizon", format!("{y} / {} bars", c.horizon))
                .input("event threshold X", format!("{} bp", r.spec.threshold_bp))
                .input("rule / complement n", format!("{} / {}", e.n, e.complement_n))
                .input("contributing sessions (rule / complement)", format!("{} / {}", c.sessions[0], c.sessions[1]))
                .input(
                    "raw n / episodes / variance-equivalent n",
                    format!("{} / {} / {}", c.raw_n, c.episodes, opt(c.effective_n, 1)),
                )
                .input("Bartlett lag L=h−1+5 / grid slots", format!("{} / {}", e.lag, e.span))
                .input("D (within-clock rule variation)", "Σ(g−ḡ_c)² over scored slots")
                .input(
                    "SE / 95% pointwise CI",
                    format!(
                        "{} / {}",
                        opt(e.se, 4),
                        e.interval
                            .map(|(a, b)| format!("{} … {}", signed(a), signed(b)))
                            .unwrap_or_else(|| "unavailable".into())
                    ),
                )
                .input("p / BY q", format!("{} / {q:.4}", opt(e.p, 4)))
                .input("testing family", "fixed m=8: mean + event × h=1,3,6,12; unavailable members p=1")
                .note("Estimator: overlap-weighted within-clock fixed-effects contrast. Clocks with no rule/complement variation contribute no identifying information. Scalar-score Newey–West Bartlett HAC on the uncompressed grid; products reset at sessions, missing expected bars and joins. Normal two-sided p; Benjamini–Yekutieli dependence correction.")
                .note("Episodes are contiguous scored-rule runs and are not independent observations. Variance-equivalent n = raw n × Var(lag 0)/Var(HAC).")
                .caveat("Full-sample descriptive contrast; not causal or predictive, may change on recomputation. Rules, symbols and thresholds tried across runs are outside this fixed family.");
                if let Some(reason) = &e.unavailable {
                    card = if e.estimate.is_some() {
                        card.note(format!("Inference withheld: {reason}. Descriptive estimate retained."))
                    } else {
                        card.unavailable(reason.clone())
                    };
                }
                card
            })
        })
        .collect();
    let probability_cards = r
        .cells
        .iter()
        .map(|c| {
            base(
                receipt,
                ts,
                "when.probability · WHEN EVENT PROBABILITY",
                "Event R[t,h] > X bp; conditional=k_rule/n_rule; base=(k_rule+k_rest)/(n_rule+n_rest); lift=conditional/base",
            )
            .value(format!(
                "conditional {} · base {} · lift {}",
                opt(c.probability, 4),
                opt(c.base_rate, 4),
                opt(c.lift, 3)
            ))
            .n(c.raw_n, "completed outcomes")
            .input("event definition", format!("simple {}-bar forward return > {} bp", c.horizon, r.spec.threshold_bp))
            .input("rule n / complement n", format!("{} / {}", c.mean_contrast.n, c.mean_contrast.complement_n))
            .input("contributing sessions (rule / complement)", format!("{} / {}", c.sessions[0], c.sessions[1]))
            .input("bootstrap seed / session rows", format!("{} / {}", r.key, r.session_rows.len()))
            .note(format!(
                "95% percentile stationary bootstrap of whole expected-session rows; {} draws; the same resampled sessions serve all four horizons; mean geometric block ceil(sessions^(1/3)); joins break blocks. Replicates lacking a group are excluded and counted; more than {} withholds the interval. No iid Wilson substitution.",
                data::DRAWS,
                data::MAX_EXCLUDED
            ))
            .caveat(format!(
                "Historical frequency, not a forecast. Inference needs ≥{} observations and ≥{} contributing sessions per group; base includes the rule group; zero base leaves lift unavailable.",
                data::MIN_GROUP,
                data::MIN_SESSIONS
            ))
        })
        .collect();
    let bucket_cards = r
        .buckets
        .iter()
        .map(|b| {
            let card = base(
                receipt,
                ts,
                "when.bucket.mean · WHEN PRIOR-QUINTILE OUTCOME",
                "edges[t]=Type-7 q20/40/60/80 of feature values strictly before t; bucket=#(edges < feature[t]); mean=ΣR/n_bucket",
            )
            .value(b.mean.map(signed).unwrap_or_else(|| "—".into()))
            .n(b.n, "completed outcomes")
            .input("feature / horizon / bucket", format!("{} / {} / Q{}", r.bucket_feature, b.horizon, b.slot + 1))
            .note(format!(
                "Needs {} prior finite feature values before edges exist; current and future values never enter an edge. All five slots retained; ties fall to the lower slot.",
                data::MIN_EDGE_HISTORY
            ))
            .caveat("Descriptive bucket mean; no bucket inference or test family.");
            if b.n == 0 { card.unavailable("no completed outcome in this bucket") } else { card }
        })
        .collect();
    let distribution_cards = r
        .cells
        .iter()
        .map(|c| {
            base(
                receipt,
                ts,
                "when.distribution · WHEN SHARED-BIN DISTRIBUTIONS",
                "mass[g,b]=count[g,b]/n[g]; 32 equal-width bins over the pooled rule ∪ complement domain; D=sup|F_rule−F_rest|",
            )
            .n(c.raw_n, "completed outcomes")
            .value(format!("rule n={} · complement n={}", c.distributions.n[0], c.distributions.n[1]))
            .input("horizon", format!("{} bars", c.horizon))
            .input("domain (bp)", format!("{} … {}", signed(c.distributions.lo), signed(c.distributions.hi)))
            .input(
                "ECDF distance D",
                c.distributions.distance.as_ref().map_or("unavailable".into(), |d| format!("{:.4}", d.d)),
            )
            .note("Each nonempty group sums to one. Complement excludes rule slots and indicator warm-up. Selected horizon changes presentation only.")
            .caveat("Descriptive; no iid KS p-value.")
        })
        .collect();
    let mut runs = vec![];
    let mut start = None;
    let step = r.tf.seconds();
    for i in 0..ts.len() {
        let rule = r.mask.values[i] == Some(1.);
        if let Some(a) = start
            && (!rule || r.mask.break_before[i])
        {
            runs.push((a, ts[i - 1] + step));
            start = None;
        }
        if rule && start.is_none() {
            start = Some(ts[i]);
        }
    }
    if let Some(a) = start {
        runs.push((a, ts[ts.len() - 1] + step));
    }
    let receipt = format!(
        "{} · CLOSED THRU {} · observed {}",
        receipt.source,
        stamp(receipt.closed_thru),
        receipt.observed_at.map(stamp).unwrap_or_else(|| "unknown clock".into())
    );
    let rule_slots = report.mask.values.iter().filter(|v| **v == Some(1.)).count();
    let shading_label = format!("RULE · {} · {rule_slots} slots", report.spec.rule());
    PreparedWhen {
        report,
        cards,
        probability_cards,
        bucket_cards,
        distribution_cards,
        runs,
        receipt,
        source_receipts: Vec::new(),
        requested_rule,
        shading_label,
    }
}

/// Single-flight cached report (default 10Y period, no receipt). Validation precedes the key and
/// cache lookup.
#[cfg(test)]
pub fn cached_report(
    input: &tr_data::studio_canvas::CanvasInput,
    rule: &str,
    threshold: f64,
) -> Result<Arc<PreparedWhen>, String> {
    cached_report_period(input, rule, threshold, data::WhenPeriod::default(), None)
}

/// As [`cached_report`] with an explicit analysis period (part of the report key) and the
/// structured source receipt for display. The period changes the key, so a report for
/// another period is never returned.
pub fn cached_report_period(
    input: &tr_data::studio_canvas::CanvasInput,
    rule: &str,
    threshold: f64,
    period: data::WhenPeriod,
    receipt: Option<tr_data::source_receipt::SourceReceipt>,
) -> Result<Arc<PreparedWhen>, String> {
    let spec = data::WhenSpec::parse(rule, threshold)?.with_period(period);
    let key = data::input_key(input, &spec)?;
    let mut cache = ReportKey::new("studio.when.v1", &input.symbol, input.tf, &input.source, &[], rule);
    cache.inputs = key;
    REPORTS.get_or_compute(cache, || {
        data::compute(input, &spec).map(|r| {
            let mut p = prepare(r, rule.into());
            p.source_receipts = receipt.into_iter().collect();
            p
        })
    })
}

pub fn cached_bootstrap(input: &PreparedWhen) -> Result<Arc<WhenBootstrap>, String> {
    input.report.mask.receipt.provenance.require_verified("When bootstrap")?;
    let mut key =
        ReportKey::new("studio.when.bootstrap.v1", &input.report.symbol, input.report.tf, "", &[], "session499");
    key.inputs = input.report.key;
    BOOTSTRAPS.get_or_compute(key, || Ok(input.report.bootstrap()))
}

pub fn cached_subset(
    input: &tr_data::studio_dist::Report,
    selection: (i64, i64),
) -> Result<Arc<PreparedSubset>, String> {
    input.provenance.require_verified("Studio subset")?;
    let span = format!("{selection:?}");
    let mut key = ReportKey::new("studio.subset.v1", &input.symbol, input.timeframe, &input.source, &[], &span);
    key.inputs = input.bars_seed;
    SUBSETS.get_or_compute(key, || {
        let r = data::subset_from_dist(input, selection)?;
        let dates = |d: Option<(i64, i64)>| d.map_or("—".into(), |(a, b)| format!("{} → {}", stamp(a), stamp(b)));
        let label = format!(
            "Selected n={} · {} · Rest n={} · {}",
            r.distribution.n[0],
            dates(r.selected_dates),
            r.distribution.n[1],
            dates(r.rest_dates)
        );
        let card = MathCard::new(
            "studio.subset.ecdf · SELECTED / REST",
            "D=sup_x|F_selected(x)−F_rest(x)|; mass[g,b]=count[g,b]/n[g]; selected=[start,end), rest=all other admitted returns",
        )
        .source(&r.receipt.source)
        .provenance(r.receipt.provenance)
        .span(stamp(input.start), stamp(input.end))
        .closed_thru(stamp(r.receipt.closed_thru))
        .input("observation clock", r.receipt.observed_at.map(stamp).unwrap_or_else(|| "unknown".into()))
        .value(format!(
            "D={} · session-block 95% {}",
            r.distribution.distance.as_ref().map_or("unavailable".into(), |d| format!("{:.4}", d.d)),
            r.distance_interval.map_or("unavailable".into(), |(a, b)| format!("{a:.4} … {b:.4}"))
        ))
        .n(r.distribution.n.iter().sum(), "one-bar log returns")
        .input("selected / rest", &label)
        .input("selection UTC [start,end)", format!("{} → {}", stamp(selection.0), stamp(selection.1)))
        .input("sessions (selected / rest) / seed", format!("{} / {} / {}", r.sessions[0], r.sessions[1], r.key))
        .input(
            "excluded replicates (no member of a group)",
            format!("{} of {} (limit {})", r.excluded, r.draws, data::MAX_EXCLUDED),
        )
        .note(format!(
            "Descriptive tie-complete ECDF distance. {} stationary whole-session bootstrap draws, mean block ceil(S^(1/3)); joins break blocks. No iid KS table. One-bar log returns (bp); missing expected intervals and overnight intraday moves excluded.",
            r.draws
        ))
        .note(
            r.unavailable
                .clone()
                .map(|u| format!("Interval unavailable: {u}"))
                .unwrap_or_else(|| "Block interval available; local-stationarity approximation.".into()),
        );
        Ok(PreparedSubset { report: r, card, label })
    })
}

pub struct WhenPane {
    pub symbol: String,
    pub tf: Timeframe,
    /// Applied rule/threshold (what the accepted report must match).
    pub condition: String,
    pub threshold: f64,
    /// Selected horizon: presentation only.
    pub horizon: usize,
    /// Applied analysis period (daily only; part of the report key and copied command).
    pub period: data::WhenPeriod,
    /// Custom-start text field (`YYYY-MM-DD`).
    pub period_draft: String,
    pub draft: String,
    pub draft_threshold: f64,
    pub generation: u64,
    pub report: Option<Arc<PreparedWhen>>,
    pub bootstrap: Option<Arc<WhenBootstrap>>,
    pub loading: bool,
    pub boot_loading: bool,
    pub error: Option<String>,
    pub boot_error: Option<String>,
}

impl WhenPane {
    pub fn new(symbol: &str, tf: Timeframe, condition: &str, threshold: f64, horizon: usize) -> Self {
        let condition =
            if condition.trim().is_empty() { data::DEFAULT_RULE.to_string() } else { condition.to_string() };
        let threshold = if threshold.is_finite() { threshold } else { data::DEFAULT_THRESHOLD_BP };
        Self {
            symbol: symbol.into(),
            tf,
            draft: condition.clone(),
            draft_threshold: threshold,
            condition,
            threshold,
            horizon: if data::HORIZONS.contains(&horizon) { horizon } else { 6 },
            period: data::WhenPeriod::default(),
            period_draft: String::new(),
            generation: studio_token(),
            report: None,
            bootstrap: None,
            loading: false,
            boot_loading: false,
            error: None,
            boot_error: None,
        }
    }
    pub fn with_period(mut self, period: data::WhenPeriod) -> Self {
        self.period = period;
        self
    }
    /// Period change: numbers clear and the next request carries the new period; responses
    /// for the previous period no longer match the fresh generation.
    pub fn set_period(&mut self, period: data::WhenPeriod) {
        if self.period != period {
            self.period = period;
            self.clear();
        }
    }
    /// Refusal/edit/context change: clear numbers, mask and overlays; old
    /// responses can no longer match the fresh token.
    pub fn clear(&mut self) {
        self.generation = studio_token();
        self.report = None;
        self.bootstrap = None;
        self.loading = false;
        self.boot_loading = false;
        self.error = None;
        self.boot_error = None;
    }
    pub fn request(&mut self, id: u64, worker: &Worker) {
        self.clear();
        self.loading = true;
        worker.send(Request::StudioWhen {
            pane: id,
            generation: self.generation,
            symbol: tr_data::security(&self.symbol).key(),
            tf: self.tf,
            rule: self.condition.clone(),
            threshold: self.threshold,
            period: self.period,
        });
    }
    pub fn accept(&mut self, generation: u64, result: Result<Arc<PreparedWhen>, String>) {
        if generation != self.generation || !self.loading {
            return;
        }
        self.loading = false;
        self.report = None;
        self.bootstrap = None;
        self.boot_loading = false;
        self.boot_error = None;
        match result {
            Ok(r)
                if tr_data::security(&r.report.symbol).key() == tr_data::security(&self.symbol).key()
                    && r.report.tf == self.tf
                    && r.requested_rule == self.condition
                    && r.report.spec.threshold_bp == self.threshold
                    && (self.tf != Timeframe::D1 || r.report.spec.period == self.period)
                    && r.report.mask.receipt.provenance.is_verified() =>
            {
                self.error = None;
                self.report = Some(r);
            }
            Ok(_) => self.error = Some("When refused: response input/provenance mismatch".into()),
            Err(e) => self.error = Some(e),
        }
    }
    pub fn accept_bootstrap(&mut self, generation: u64, key: u64, result: Result<Arc<WhenBootstrap>, String>) {
        let expected = self.report.as_ref().map(|r| r.report.key);
        if generation != self.generation || !self.boot_loading || expected != Some(key) {
            return;
        }
        self.boot_loading = false;
        match result {
            Ok(r) if r.key == key => self.bootstrap = Some(r),
            Ok(_) => self.boot_error = Some("bootstrap key mismatch".into()),
            Err(e) => {
                self.bootstrap = None;
                self.boot_error = Some(e);
            }
        }
    }
    /// Accepted rule runs for canvas shading, only while the report matches the applied rule.
    pub fn shading(&self) -> Option<(&[(i64, i64)], &str)> {
        let r = self.report.as_ref()?;
        (r.requested_rule == self.condition
            && r.report.spec.threshold_bp == self.threshold
            && (self.tf != Timeframe::D1 || r.report.spec.period == self.period))
            .then(|| (r.runs.as_slice(), r.shading_label.as_str()))
    }
    fn apply_draft(&mut self) -> Result<(), String> {
        data::WhenSpec::parse(&self.draft, self.draft_threshold)?;
        self.condition = self.draft.trim().to_string();
        self.threshold = self.draft_threshold;
        self.clear();
        Ok(())
    }
}

pub(super) fn show(ui: &mut Ui, id: u64, p: &mut WhenPane, worker: &Worker) -> Vec<crate::congress_view::Action> {
    let mut run = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("WHEN").font(theme::ui_font(13.)).strong());
        let edit = ui.add(egui::TextEdit::singleline(&mut p.draft).desired_width(260.).font(theme::mono(12.)));
        run |= edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.label(RichText::new("X bp").font(theme::ui_font(11.)));
        ui.add(egui::DragValue::new(&mut p.draft_threshold).speed(1.).range(-10_000.0..=10_000.0));
        run |= ui.button("Run rule").clicked();
    });
    // Parsed echo of the draft (bounded string parse; no indicator work here).
    let echo = match data::WhenSpec::parse(&p.draft, p.draft_threshold) {
        Ok(s) => format!("Parsed: {} · event R > {} bp · h ∈ {{1,3,6,12}} fixed", s.rule(), s.threshold_bp),
        Err(e) => format!("Rule error: {e}"),
    };
    ui.label(RichText::new(echo).font(theme::mono(10.)).color(theme::muted()));
    if p.tf == Timeframe::D1 {
        period_row(ui, p);
    }
    let edited = p.draft.trim() != p.condition || p.draft_threshold != p.threshold;
    if run {
        match p.apply_draft() {
            Ok(()) => p.request(id, worker),
            Err(e) => p.error = Some(e),
        }
    } else if p.report.is_none() && !p.loading && p.error.is_none() {
        p.request(id, worker);
    }
    if edited && p.report.is_some() {
        ui.label(
            RichText::new("Draft differs from the accepted rule · Run rule to replace this snapshot")
                .font(theme::ui_font(10.))
                .color(theme::caution()),
        );
    }
    if let Some(e) = p.error.clone() {
        if e.starts_with(data::PERIOD_LIMIT_PREFIX) {
            // Not a failure: the period is simply too long for the analysis grid.
            let mut pick = None;
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{} ({}). Use a shorter period:", data::PERIOD_LIMIT_PREFIX, p.period.label()))
                        .font(theme::ui_font(11.))
                        .color(theme::caution()),
                );
                for y in data::WhenPeriod::PRESETS {
                    let candidate = data::WhenPeriod::Years(y);
                    if candidate != p.period && ui.small_button(format!("{y}Y")).clicked() {
                        pick = Some(candidate);
                    }
                }
            });
            if let Some(period) = pick {
                p.set_period(period);
            }
        } else {
            ui.colored_label(theme::alert(), format!("When unavailable: {e}"));
        }
    }
    if p.loading {
        ui.allocate_ui(vec2(ui.available_width(), BODY_HEIGHT), |ui| {
            ui.label("Loading verified closed bars and fixed-family evidence…");
            ui.allocate_space(vec2(ui.available_width(), BODY_HEIGHT - 20.));
        });
    }
    if let Some(r) = p.report.clone() {
        if p.bootstrap.is_none() && !p.boot_loading && p.boot_error.is_none() {
            p.boot_loading = true;
            worker.send(Request::StudioWhenBootstrap { pane: id, generation: p.generation, input: r.clone() });
        }
        let boot = p.bootstrap.clone();
        let boot_error = p.boot_error.clone();
        egui::ScrollArea::vertical().id_salt((id, "when-scroll")).show(ui, |ui| {
            view(ui, &r, boot.as_deref(), boot_error.as_deref(), &mut p.horizon);
        });
    }
    vec![]
}

/// Disclosed analysis period: 5Y / 10Y / 20Y / Custom start date. Daily only.
fn period_row(ui: &mut Ui, p: &mut WhenPane) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("PERIOD").font(theme::ui_font(10.)).color(theme::muted()));
        let mut pick = None;
        for y in data::WhenPeriod::PRESETS {
            let candidate = data::WhenPeriod::Years(y);
            if ui.selectable_label(p.period == candidate, format!("{y}Y")).clicked() {
                pick = Some(candidate);
            }
        }
        let custom = matches!(p.period, data::WhenPeriod::Since(_));
        let edit = ui.add(
            egui::TextEdit::singleline(&mut p.period_draft)
                .hint_text("Custom YYYY-MM-DD")
                .desired_width(120.)
                .font(theme::mono(11.)),
        );
        if custom {
            ui.label(RichText::new("●").font(theme::ui_font(9.)).color(theme::accent()));
        }
        // Invalid input is a hint beside the field only: the accepted report (and its numbers)
        // stays untouched and no pane-level error appears.
        if !p.period_draft.trim().is_empty() {
            match data::WhenPeriod::parse(p.period_draft.trim()) {
                Ok(d @ data::WhenPeriod::Since(_)) => {
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        pick = Some(d);
                    }
                }
                _ => {
                    ui.label(
                        RichText::new("start date as YYYY-MM-DD").font(theme::ui_font(10.)).color(theme::caution()),
                    );
                }
            }
        }
        if let Some(w) = p.report.as_ref().and_then(|r| r.report.window.as_ref()) {
            ui.label(RichText::new(window_line(w, p.report.as_ref().map_or(0, |r| r.report.sessions))).font(theme::mono(10.)).color(theme::muted()));
        }
        if let Some(period) = pick {
            p.set_period(period);
        }
    });
}

/// `Analysis period 10Y · Nov 21, 2016 → Nov 20, 2026 · 2,517 sessions · 1,000 warm-up bars not scored`.
pub(super) fn window_line(w: &data::WhenWindow, sessions: usize) -> String {
    let d = |t: i64| stamp(t).split(' ').next().unwrap_or_default().to_string();
    format!(
        "Analysis period {} · {} → {} · {} sessions{}{}",
        w.period.label(),
        d(w.scored_from),
        d(w.scored_to),
        tr_data::source_receipt::thousands(sessions),
        if w.warmup_bars > 0 {
            format!(" · {} warm-up bars not scored", tr_data::source_receipt::thousands(w.warmup_bars))
        } else {
            String::new()
        },
        if w.clipped { " · history starts later than requested" } else { "" }
    )
}

/// Two normalised shared-bin distributions with a printed legend.
pub(super) fn histogram(ui: &mut Ui, d: &SharedDistribution, card: &MathCard, labels: [&str; 2]) -> Rect {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("NORMALISED SHARED-BIN DISTRIBUTIONS · mass/bin")
                .font(theme::ui_font(10.))
                .color(theme::muted()),
        );
        for (g, name) in labels.iter().enumerate() {
            ui.label(RichText::new(format!("■ {name} n={}", d.n[g])).font(theme::mono(10.)).color(theme::series(g)));
        }
    });
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width().max(40.), 96.), Sense::hover());
    let plot = rect.shrink2(vec2(8., 14.));
    let painter = ui.painter_at(rect);
    let max = d.mass.iter().flatten().copied().fold(0f64, f64::max).max(1e-12);
    let w = plot.width() / data::BINS as f32;
    for g in 0..2 {
        for (i, m) in d.mass[g].iter().enumerate() {
            if *m <= 0. {
                continue;
            }
            let x = plot.left() + i as f32 * w + g as f32 * w * 0.5;
            let top = plot.bottom() - (*m / max) as f32 * plot.height();
            painter.rect_filled(
                Rect::from_min_max(pos2(x, top), pos2(x + w * 0.5, plot.bottom())),
                0.,
                theme::series(g).gamma_multiply(0.7),
            );
        }
    }
    if d.lo < 0. && d.hi > 0. {
        let x = plot.left() + ((-d.lo) / (d.hi - d.lo)) as f32 * plot.width();
        painter.vline(x, plot.y_range(), Stroke::new(1., theme::muted()));
    }
    painter.text(
        rect.left_bottom(),
        egui::Align2::LEFT_BOTTOM,
        format!("{} bp", signed(d.lo)),
        theme::mono(10.),
        theme::muted(),
    );
    painter.text(
        rect.right_bottom(),
        egui::Align2::RIGHT_BOTTOM,
        format!("{} bp", signed(d.hi)),
        theme::mono(10.),
        theme::muted(),
    );
    response.math_card(|| card.clone());
    rect
}

/// Paint the accepted report. Returns the first evidence cell (hover probe).
pub(super) fn view(
    ui: &mut Ui,
    p: &PreparedWhen,
    boot: Option<&WhenBootstrap>,
    boot_error: Option<&str>,
    horizon: &mut usize,
) -> Rect {
    let r = &p.report;
    // The source chip lives in the Studio header (When is only ever hosted there); the lens
    // body keeps just the window line.
    if let Some(w) = &r.window {
        ui.label(RichText::new(window_line(w, r.sessions)).font(theme::mono(10.)).color(theme::muted()));
    }
    ui.label(
        RichText::new(format!(
            "RULE {} · FIXED FAMILY m=8 (mean + event × h=1,3,6,12) · BY q · within-clock HAC",
            r.spec.rule()
        ))
        .font(theme::ui_font(11.))
        .strong(),
    );
    // Quiet header; units in column headers.
    let (head, _) = ui.allocate_exact_size(vec2(ui.available_width().max(40.), 16.), Sense::hover());
    let cols = columns(head);
    let painter = ui.painter_at(head);
    for (x, text) in [
        (cols[0], "h bars"),
        (cols[1], "Δ mean bp · 95% HAC"),
        (cols[2], "Δ P(R>X) pp · 95% HAC"),
        (cols[3], "q mean / event"),
        (cols[4], "raw n / episodes / n_eff"),
    ] {
        painter.text(pos2(x, head.center().y), egui::Align2::LEFT_CENTER, text, theme::ui_font(10.), theme::muted());
    }
    let scales = [0, 1].map(|j| {
        r.cells
            .iter()
            .flat_map(|c| {
                let e = if j == 0 { &c.mean_contrast } else { &c.hit_contrast };
                e.interval.map(|(a, b)| [a, b]).into_iter().flatten().chain(e.estimate)
            })
            .fold(1e-9f64, |m, v| m.max(v.abs()))
    });
    let mut probe = Rect::NOTHING;
    for (i, c) in r.cells.iter().enumerate() {
        let (row, response) = ui.allocate_exact_size(vec2(ui.available_width().max(40.), 30.), Sense::click());
        if response.clicked() {
            *horizon = c.horizon;
        }
        let cols = columns(row);
        let painter = ui.painter_at(row);
        if *horizon == c.horizon {
            painter.vline(row.left() + 1.5, row.y_range(), Stroke::new(3., theme::text()));
        }
        painter.text(
            pos2(cols[0], row.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{:>2}", c.horizon),
            theme::mono(12.),
            theme::text(),
        );
        for (j, e) in [&c.mean_contrast, &c.hit_contrast].into_iter().enumerate() {
            let left = cols[1 + j];
            let width = (cols[2 + j] - left - 8.).max(30.);
            let cell = Rect::from_min_size(pos2(left, row.top()), vec2(width, row.height()));
            let k = if j == 0 { 1. } else { 100. };
            let value = e.estimate.map(|v| signed(v * k)).unwrap_or_else(|| "—".into());
            painter.text(
                pos2(left, row.top() + 9.),
                egui::Align2::LEFT_CENTER,
                value,
                theme::mono(11.),
                e.estimate.map_or(theme::muted(), theme::sign),
            );
            // CI whisker on a shared per-column scale; zero tick muted.
            let map = |v: f64| left + width * 0.5 + (v / scales[j]) as f32 * width * 0.48;
            let y = row.bottom() - 8.;
            painter.vline(map(0.), y - 4. ..=y + 4., Stroke::new(1., theme::muted()));
            if let (Some((a, b)), Some(x)) = (e.interval, e.estimate) {
                painter.hline(map(a)..=map(b), y, Stroke::new(1., theme::muted()));
                painter.circle_filled(pos2(map(x), y), 2.5, theme::sign(x));
            } else if let Some(x) = e.estimate {
                painter.circle_stroke(pos2(map(x), y), 2.5, Stroke::new(1., theme::muted()));
            }
            let hover = ui.interact(cell, ui.id().with(("when-cell", i, j)), Sense::hover());
            let card = &p.cards[i][j];
            hover.math_card(|| card.clone());
            if i == 0 && j == 0 {
                probe = cell;
            }
        }
        painter.text(
            pos2(cols[3], row.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{:.3} / {:.3}", c.q_mean, c.q_hit),
            theme::mono(11.),
            theme::text(),
        );
        painter.text(
            pos2(cols[4], row.center().y),
            egui::Align2::LEFT_CENTER,
            format!(
                "{} / {} / {}",
                c.raw_n,
                c.episodes,
                c.effective_n.map(|v| format!("{v:.0}")).unwrap_or_else(|| "—".into())
            ),
            theme::mono(11.),
            theme::text(),
        );
        // Significance: neutral outline only; LOW N: caution corner tick.
        if c.q_mean <= 0.1 || c.q_hit <= 0.1 {
            painter.rect_stroke(row.shrink(1.), 0., Stroke::new(1., theme::text()), egui::StrokeKind::Inside);
        }
        if c.mean_contrast.p.is_none() || c.hit_contrast.p.is_none() {
            let corner = row.right_top();
            painter.line_segment([corner + vec2(-7., 0.), corner + vec2(0., 7.)], Stroke::new(2., theme::caution()));
        }
    }
    let k = data::HORIZONS.iter().position(|h| h == horizon).unwrap_or(2);
    let c = &r.cells[k];
    ui.add_space(4.);
    ui.label(
        RichText::new(format!("SELECTED h={} · rule vs complement", c.horizon)).font(theme::ui_font(11.)).strong(),
    );
    histogram(ui, &c.distributions, &p.distribution_cards[k], ["Rule", "Complement"]);
    // Probability tile: conditional, base, lift, block-bootstrap interval.
    let ci = boot.and_then(|b| b.probability[k]);
    let lift_ci = boot.and_then(|b| b.lift[k]);
    let reason = match (boot, boot_error) {
        (_, Some(e)) => format!("block CI unavailable: {e}"),
        (Some(b), _) => b.unavailable[k].clone().unwrap_or_default(),
        (None, None) => "block CI pending".into(),
    };
    let pct = |v: Option<f64>| v.map(|v| format!("{:.2}%", 100. * v)).unwrap_or_else(|| "—".into());
    let interval = ci.map(|(a, b)| format!("block 95% {:.2}…{:.2}%", 100. * a, 100. * b)).unwrap_or(reason.clone());
    let tile = format!(
        "P(R>{}bp | rule) {} · base {} · lift {} · {} · n={}/{}",
        r.spec.threshold_bp,
        pct(c.probability),
        pct(c.base_rate),
        c.lift.map(|v| format!("{v:.2}×")).unwrap_or_else(|| "—".into()),
        interval,
        c.mean_contrast.n,
        c.raw_n
    );
    let card = p.probability_cards[k].clone();
    let block = boot.map_or(0, |b| b.block);
    let excluded = boot.map_or(0, |b| b.excluded[k]);
    ui.label(RichText::new(tile).font(theme::mono(11.))).math_card(move || {
        card.input(
            "95% conditional / lift interval",
            format!(
                "{} / {}",
                ci.map_or("unavailable".into(), |(a, b)| format!("{a:.4} … {b:.4}")),
                lift_ci.map_or("unavailable".into(), |(a, b)| format!("{a:.3} … {b:.3}"))
            ),
        )
        .note(format!("Mean block length {block} sessions · excluded replicates {excluded}. {reason}"))
    });
    // Compact five-bucket outcome strip (descriptive, signed ink).
    ui.label(
        RichText::new(format!("PRIOR-ONLY {} QUINTILES · mean R bp (h={})", r.bucket_feature, c.horizon))
            .font(theme::ui_font(10.))
            .color(theme::muted()),
    );
    let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width().max(40.), 22.), Sense::hover());
    let painter = ui.painter_at(strip);
    let w = strip.width() / 5.;
    for (i, b) in r.buckets.iter().enumerate().filter(|(_, b)| b.horizon == c.horizon) {
        let cell =
            Rect::from_min_size(pos2(strip.left() + b.slot as f32 * w, strip.top()), vec2(w - 2., strip.height()));
        painter.rect_stroke(cell, 0., Stroke::new(1., theme::muted().gamma_multiply(0.4)), egui::StrokeKind::Inside);
        painter.text(
            cell.left_center() + vec2(4., 0.),
            egui::Align2::LEFT_CENTER,
            format!("Q{} {} n={}", b.slot + 1, b.mean.map(signed).unwrap_or_else(|| "—".into()), b.n),
            theme::mono(10.),
            b.mean.map_or(theme::muted(), theme::sign),
        );
        let card = &p.bucket_cards[i];
        ui.interact(cell, ui.id().with(("when-bucket", i)), Sense::hover()).math_card(|| card.clone());
    }
    ui.label(
        RichText::new(format!(
            "LOW N keeps descriptive values · inference needs ≥{}/group and ≥{} sessions/group · Bartlett L=h−1+5 · pointwise 95% · episodes ≠ independent n",
            data::MIN_GROUP,
            data::MIN_SESSIONS
        ))
        .font(theme::ui_font(10.))
        .color(theme::muted()),
    );
    probe
}

fn columns(row: Rect) -> [f32; 6] {
    let w = row.width();
    let x = row.left() + 48.;
    [row.left() + 8., x, x + w * 0.27, x + w * 0.54, x + w * 0.68, row.right()]
}

/// Selected-range comparison in Distribution: shared bins, counts and dates.
pub(super) fn subset_view(ui: &mut Ui, p: &PreparedSubset) -> Rect {
    ui.label(
        RichText::new("SELECTED RANGE vs REST · closed one-bar log returns (bp)").font(theme::ui_font(11.)).strong(),
    );
    ui.label(RichText::new(&p.label).font(theme::mono(10.))).math_card(|| p.card.clone());
    let rect = histogram(ui, &p.report.distribution, &p.card, ["Selected", "Rest"]);
    let d = p.report.distribution.distance.as_ref().map(|d| format!("{:.4}", d.d)).unwrap_or_else(|| "—".into());
    let interval = p
        .report
        .distance_interval
        .map(|(a, b)| format!("session-block 95% {a:.4}…{b:.4}"))
        .unwrap_or_else(|| p.report.unavailable.clone().unwrap_or_else(|| "interval unavailable".into()));
    ui.label(RichText::new(format!("ECDF D {d} · {interval}")).font(theme::mono(10.))).math_card(|| p.card.clone());
    rect
}

#[cfg(test)]
pub(crate) fn fixture_report(sessions: usize) -> Arc<PreparedWhen> {
    let input = tr_perf::fixtures::when_input(sessions);
    cached_report(&input, "RSI(14)<45", 0.).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane_with(report: Arc<PreparedWhen>) -> WhenPane {
        let mut p = WhenPane::new("AAPL US Equity", Timeframe::M5, "RSI(14)<45", 0., 6);
        p.loading = true;
        let g = p.generation;
        p.accept(g, Ok(report));
        p
    }

    #[test]
    fn rule_shading_clears_on_refusal() {
        let r = fixture_report(40);
        assert!(!r.runs.is_empty());
        let mut p = pane_with(r.clone());
        let (runs, label) = p.shading().expect("accepted mask shades");
        assert_eq!(runs.len(), r.runs.len());
        assert!(label.contains("RSI"));
        // Runs come from the accepted mask: every run starts on a rule slot.
        for (a, b) in runs {
            assert!(a < b);
            let i = r.report.mask.ts.binary_search(a).unwrap();
            assert_eq!(r.report.mask.values[i], Some(1.));
        }
        // A refusal for the next request clears numbers, mask and shading.
        let (w, mut rx) = crate::worker::Worker::test_channel();
        p.request(1, &w);
        assert!(matches!(rx.try_recv(), Ok(Request::StudioWhen { .. })));
        assert!(p.shading().is_none() && p.report.is_none());
        let g = p.generation;
        p.accept(g, Err("When: sample mode has no research data".into()));
        assert!(p.shading().is_none() && p.report.is_none());
        assert!(p.error.as_deref().unwrap().contains("sample"));
        // A stale success for an old token cannot restore the overlay.
        p.accept(g.wrapping_sub(1), Ok(r.clone()));
        p.accept(g, Ok(r));
        assert!(p.shading().is_none(), "late/duplicate responses are ignored once settled");
    }

    #[test]
    fn when_stale_and_mismatched_responses_are_ignored() {
        let r = fixture_report(40);
        let mut p = WhenPane::new("AAPL US Equity", Timeframe::M5, "RSI(14)<30", 0., 6);
        p.loading = true;
        let g = p.generation;
        p.accept(g, Ok(r.clone()));
        assert!(p.report.is_none() && p.error.as_deref().unwrap().contains("mismatch"), "rule mismatch refused");
        let mut p = pane_with(r.clone());
        let key = r.report.key;
        p.boot_loading = true;
        let boot = cached_bootstrap(&r).unwrap();
        p.accept_bootstrap(p.generation, key ^ 1, Ok(boot.clone()));
        assert!(p.bootstrap.is_none(), "wrong key ignored");
        p.accept_bootstrap(p.generation + 1, key, Ok(boot.clone()));
        assert!(p.bootstrap.is_none(), "wrong token ignored");
        p.accept_bootstrap(p.generation, key, Ok(boot));
        assert!(p.bootstrap.is_some());
        // Editing the draft keeps the accepted snapshot until Run.
        p.draft = "RSI(14)<20".into();
        assert!(p.shading().is_some());
        p.apply_draft().unwrap();
        assert!(p.shading().is_none() && p.report.is_none());
    }

    #[test]
    fn when_cards_print_ids_inputs_and_receipts() {
        let r = fixture_report(40);
        let t = r.cards[0][0].to_text();
        let words = ["when.contrast.mean", "Bartlett lag", "CLOSED THRU", "observation clock", "fixed m=8", "episodes"];
        for word in words {
            assert!(t.contains(word), "{word}: {t}");
        }
        assert!(r.probability_cards[0].to_text().contains("event definition"));
        assert_eq!(r.bucket_cards.len(), 20);
        assert_eq!(r.cards.len(), 4);
    }
}

#[cfg(test)]
mod period_tests;
