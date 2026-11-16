//! Session-return inference and time-ordered evaluation. No I/O, selection, or paint work.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use tr_core::Bar;

pub const METHOD: &str = "honest-bt-v1";
pub const MAX_TRIALS: usize = 64;
pub const MAX_OPERATIONS: usize = 60_000_000;
pub const YEAR: f64 = 252.0;
pub const CORRELATION_THRESHOLD: f64 = 0.90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HonestySettings {
    pub folds: usize,
    pub embargo_sessions: usize,
}
impl Default for HonestySettings {
    fn default() -> Self {
        Self { folds: 3, embargo_sessions: 1 }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Reason {
    TooFewReturns,
    ZeroVariance,
    InvalidNumeric,
    InvalidProbabilityVariance,
    Ruin,
    InsufficientHistory { available: usize, required: usize },
    InvalidSettings,
    UnresolvedHoldingLabel,
    NonpositiveIsSharpe,
    NoComparableTrialWindow,
    RegisterUnavailable,
    IncompleteFamily,
    WorkBudgetExceeded { trials: usize },
    DailyGap,
    ReplayFailed(String),
}
impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewReturns => write!(f, "too few session returns"),
            Self::ZeroVariance => write!(f, "zero return variance"),
            Self::InvalidNumeric => write!(f, "invalid or overflowing numeric input"),
            Self::InvalidProbabilityVariance => write!(f, "nonpositive PSR variance"),
            Self::Ruin => write!(f, "equity reached zero or below; simulation stopped"),
            Self::InsufficientHistory { available, required } => {
                write!(f, "need {required} sessions; have {available}")
            }
            Self::InvalidSettings => write!(f, "invalid fold/cost/strategy settings"),
            Self::UnresolvedHoldingLabel => write!(f, "unresolved holding label leaves insufficient IS history"),
            Self::NonpositiveIsSharpe => write!(f, "IS Sharpe is nonpositive or too close to zero"),
            Self::NoComparableTrialWindow => write!(f, "insufficient common trial dates"),
            Self::RegisterUnavailable => write!(f, "trial history unavailable"),
            Self::IncompleteFamily => write!(f, "incomplete trial family (failed or nonzero constant series)"),
            Self::WorkBudgetExceeded { trials } => {
                write!(f, "family replay work budget exceeded ({trials} tracked experiments)")
            }
            Self::DailyGap => write!(
                f,
                "daily history has a missing, duplicate, invalid or non-session bar; holding returns cannot bridge a hole"
            ),
            Self::ReplayFailed(s) => write!(f, "experiment replay failed: {s}"),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Estimate<T> {
    Available(T),
    Unavailable(Reason),
}
impl<T> Estimate<T> {
    pub fn value(&self) -> Option<&T> {
        if let Self::Available(v) = self { Some(v) } else { None }
    }
    pub fn reason(&self) -> Option<&Reason> {
        if let Self::Unavailable(r) = self { Some(r) } else { None }
    }
}
impl<T> From<Result<T, Reason>> for Estimate<T> {
    fn from(r: Result<T, Reason>) -> Self {
        match r {
            Ok(v) => Self::Available(v),
            Err(e) => Self::Unavailable(e),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReturnPoint {
    pub session_date: String,
    pub period_start: i64,
    pub period_end: i64,
    pub net_return: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HoldingLabel {
    pub decision_ts: i64,
    pub entry_ts: i64,
    pub exit_ts: Option<i64>,
    /// Inclusive accounting intervals: reversal-day returns can touch two labels.
    pub first_return: usize,
    pub last_return: usize,
}
#[derive(Debug, Clone, Default)]
pub struct Tape {
    pub returns: Vec<ReturnPoint>,
    pub labels: Vec<HoldingLabel>,
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Moments {
    pub n: usize,
    pub mean: f64,
    pub sample_sd: f64,
    pub sharpe: f64,
    pub skew: f64,
    pub pearson_kurtosis: f64,
    pub probability_variance: f64,
}
/// Center after translation and scaling: avoid catastrophic raw-power sums and
/// preserve scale invariance even for small returns. Never drop a bad observation.
pub fn moments(xs: &[f64]) -> Result<Moments, Reason> {
    if xs.len() < 2 {
        return Err(Reason::TooFewReturns);
    }
    if xs.iter().any(|x| !x.is_finite()) {
        return Err(Reason::InvalidNumeric);
    }
    let base = xs[0];
    let deltas: Vec<_> = xs.iter().map(|x| x - base).collect();
    if deltas.iter().any(|x| !x.is_finite()) {
        return Err(Reason::InvalidNumeric);
    }
    let scale = deltas.iter().fold(0.0_f64, |a, x| a.max(x.abs()));
    if scale == 0. {
        return Err(Reason::ZeroVariance);
    }
    let mean_delta = deltas.iter().map(|x| x / scale).sum::<f64>() / xs.len() as f64;
    let mean = base + mean_delta * scale;
    let (mut m2, mut m3, mut m4) = (0., 0., 0.);
    for x in &deltas {
        let d = x / scale - mean_delta;
        m2 += d * d;
        m3 += d.powi(3);
        m4 += d.powi(4);
    }
    if m2 == 0.0 {
        return Err(Reason::ZeroVariance);
    }
    let n = xs.len() as f64;
    let sd_scaled = (m2 / (n - 1.)).sqrt();
    let variance = m2 / n;
    let mut out = Moments {
        probability_variance: 0.,
        n: xs.len(),
        mean,
        sample_sd: sd_scaled * scale,
        sharpe: mean / (sd_scaled * scale),
        skew: (m3 / n) / variance.powf(1.5),
        pearson_kurtosis: (m4 / n) / variance.powi(2),
    };
    out.probability_variance = psr_variance(&out);
    if [out.mean, out.sample_sd, out.sharpe, out.skew, out.pearson_kurtosis, out.probability_variance]
        .iter()
        .any(|v| !v.is_finite())
        || out.sample_sd <= 0.
    {
        return Err(Reason::InvalidNumeric);
    }
    Ok(out)
}
pub fn psr_variance(m: &Moments) -> f64 {
    1. - m.skew * m.sharpe + (m.pearson_kurtosis - 1.) * m.sharpe.powi(2) / 4.
}
pub fn psr(m: &Moments, benchmark: f64) -> Result<f64, Reason> {
    if m.n < 4 {
        return Err(Reason::TooFewReturns);
    }
    let v = psr_variance(m);
    if !v.is_finite() || v <= 0. {
        return Err(Reason::InvalidProbabilityVariance);
    }
    if !benchmark.is_finite() {
        return Err(Reason::InvalidNumeric);
    }
    let z = (m.sharpe - benchmark) * ((m.n - 1) as f64).sqrt() / v.sqrt();
    if !z.is_finite() {
        return Err(Reason::InvalidNumeric);
    }
    Ok(tr_core::options::norm_cdf(z))
}
pub fn expected_maximum(neff: usize, dispersion: f64) -> Result<f64, Reason> {
    if neff == 0 || !dispersion.is_finite() || dispersion < 0. {
        return Err(Reason::InvalidNumeric);
    }
    let a = match neff {
        1 => 0.,
        2 => 1. / std::f64::consts::PI.sqrt(),
        _ => {
            let n = neff as f64;
            #[allow(clippy::approx_constant)] // pinned paper constant; keep older compiler compatibility
            let gamma = 0.577_215_664_901_532_9;
            (1. - gamma) * crate::studio_dist::normal_quantile(1. - 1. / n)
                + gamma * crate::studio_dist::normal_quantile(1. - 1. / (n * std::f64::consts::E))
        }
    };
    let out = dispersion * a;
    if out.is_finite() { Ok(out) } else { Err(Reason::InvalidNumeric) }
}
/// Complete linkage; inputs MUST already be sorted by canonical experiment ID.
/// Negative correlation is never evidence of a duplicate opportunity.
pub fn clusters(columns: &[Vec<f64>]) -> Result<Vec<Vec<usize>>, Reason> {
    let ms: Vec<_> = columns.iter().map(|x| moments(x)).collect::<Result<_, _>>()?;
    let mut corr = vec![vec![0.; columns.len()]; columns.len()];
    for i in 0..columns.len() {
        for j in i + 1..columns.len() {
            if columns[i].len() != columns[j].len() {
                return Err(Reason::InvalidNumeric);
            }
            // Standardize before products so covariance cannot overflow.
            let c = columns[i]
                .iter()
                .zip(&columns[j])
                .map(|(x, y)| ((x - ms[i].mean) / ms[i].sample_sd) * ((y - ms[j].mean) / ms[j].sample_sd))
                .sum::<f64>()
                / (columns[i].len() - 1) as f64;
            if !c.is_finite() {
                return Err(Reason::InvalidNumeric);
            }
            corr[i][j] = c.clamp(-1., 1.);
            corr[j][i] = corr[i][j];
        }
    }
    let mut groups: Vec<Vec<usize>> = (0..columns.len()).map(|i| vec![i]).collect();
    loop {
        let mut best = None;
        let mut score = CORRELATION_THRESHOLD;
        for a in 0..groups.len() {
            for b in a + 1..groups.len() {
                let min = groups[a].iter().flat_map(|i| groups[b].iter().map(|j| corr[*i][*j])).fold(1., f64::min);
                if min >= CORRELATION_THRESHOLD && (best.is_none() || min > score) {
                    best = Some((a, b));
                    score = min;
                }
            }
        }
        let Some((a, b)) = best else { break };
        let other = groups.remove(b);
        groups[a].extend(other);
        groups[a].sort_unstable();
        groups.sort();
    }
    Ok(groups)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexSpan {
    pub start: usize,
    pub end: usize,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub indices: IndexSpan,
    pub start: i64,
    pub end: i64,
    pub first_date: String,
    pub last_date: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub train: IndexSpan,
    pub test: IndexSpan,
    pub embargo: IndexSpan,
    pub nominal_embargo_end: usize,
}
pub fn schedule(
    units: usize,
    warmup: usize,
    intraday: bool,
    settings: HonestySettings,
) -> Result<Vec<Schedule>, Reason> {
    let k = settings.folds;
    if !(2..=6).contains(&k) || settings.embargo_sessions > 252 {
        return Err(Reason::InvalidSettings);
    }
    let l = if intraday { 20 } else { 60.max(warmup.saturating_add(30)) };
    let minimum = if intraday { 5 } else { 20 };
    let reserved = (k - 1) * settings.embargo_sessions;
    let need = l.checked_add(reserved).and_then(|n| n.checked_add(k * minimum)).ok_or(Reason::InvalidSettings)?;
    if units < need {
        return Err(Reason::InsufficientHistory { available: units, required: need });
    }
    let r = units - l - reserved;
    let mut start = l;
    let mut out = Vec::new();
    for f in 0..k {
        let end = start + r / k + usize::from(f < r % k);
        out.push(Schedule {
            train: IndexSpan { start: warmup, end: start },
            test: IndexSpan { start, end },
            embargo: IndexSpan { start: end, end: (end + settings.embargo_sessions).min(units) },
            nominal_embargo_end: end + settings.embargo_sessions,
        });
        start = end + settings.embargo_sessions;
    }
    Ok(out)
}
pub fn span(units: &[ReturnPoint], s: IndexSpan) -> Span {
    Span {
        indices: s,
        start: units[s.start].period_start,
        end: units[s.end - 1].period_end,
        first_date: units[s.start].session_date.clone(),
        last_date: units[s.end - 1].session_date.clone(),
    }
}
/// Build observation masks from causal prefix labels, never from future exits.
/// A dependent return that mixes labels is removed in full, including reversal costs.
pub fn purge(tape: &Tape, candidate: IndexSpan, test: (i64, i64), embargoes: &[(i64, i64)]) -> (Vec<bool>, usize) {
    let mut keep = vec![false; tape.returns.len()];
    for v in keep.iter_mut().take(candidate.end).skip(candidate.start) {
        *v = true;
    }
    let mut removed_labels = 0;
    for label in &tape.labels {
        if label_touches_candidate(label, candidate) && label_is_purged(label, test, embargoes) {
            removed_labels += 1;
            for v in keep.iter_mut().take(label.last_return + 1).skip(label.first_return) {
                *v = false;
            }
        }
    }
    for (i, p) in tape.returns.iter().enumerate() {
        if embargoes.iter().any(|(a, b)| p.period_start <= *b && p.period_end >= *a) {
            keep[i] = false;
        }
    }
    (keep, removed_labels)
}
fn label_touches_candidate(label: &HoldingLabel, candidate: IndexSpan) -> bool {
    label.first_return < candidate.end && label.last_return >= candidate.start
}
fn label_is_purged(label: &HoldingLabel, test: (i64, i64), embargoes: &[(i64, i64)]) -> bool {
    let overlaps = |a, b| label.decision_ts <= b && label.exit_ts.is_none_or(|exit| exit >= a);
    label.exit_ts.is_none() || overlaps(test.0, test.1) || embargoes.iter().any(|(a, b)| overlaps(*a, *b))
}
pub fn mask_spans(units: &[ReturnPoint], mask: &[bool], wanted: bool, candidate: IndexSpan) -> Vec<Span> {
    let mut out = Vec::new();
    let mut i = candidate.start;
    while i < candidate.end {
        if mask[i] != wanted {
            i += 1;
            continue;
        }
        let start = i;
        while i < candidate.end && mask[i] == wanted {
            i += 1;
        }
        out.push(span(units, IndexSpan { start, end: i }));
    }
    out
}
pub fn retention(is: &Estimate<Moments>, oos: &Estimate<Moments>) -> Estimate<f64> {
    match (is.value(), oos.value()) {
        (Some(a), Some(b)) if a.sharpe > 1e-12 => Estimate::Available(b.sharpe / a.sharpe),
        (Some(_), Some(_)) => Estimate::Unavailable(Reason::NonpositiveIsSharpe),
        _ => Estimate::Unavailable(is.reason().or(oos.reason()).cloned().unwrap_or(Reason::TooFewReturns)),
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FoldReport {
    pub train_candidate: Span,
    pub train_used: Vec<Span>,
    pub purged: Vec<Span>,
    pub prior_embargoes: Vec<Span>,
    pub test: Span,
    pub embargo: Option<Span>,
    pub nominal_embargo_end: usize,
    pub removed_labels: usize,
    pub purged_labels: Vec<HoldingLabel>,
    pub train_n: usize,
    pub test_n: usize,
    pub train: Estimate<Moments>,
    pub oos: Estimate<Moments>,
    pub retention: Estimate<f64>,
    pub net_return: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EquityPoint {
    pub ts: i64,
    pub equity: f64,
    pub fold: usize,
    pub break_before: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evaluation {
    pub returns: Vec<ReturnPoint>,
    pub folds: Vec<FoldReport>,
    pub equity: Vec<EquityPoint>,
    pub equity_bounds: [f64; 2],
    pub scatter_bound: f64,
    pub max_drawdown: f64,
    pub full_oos: Estimate<Moments>,
    pub retention: Estimate<f64>,
}
pub fn evaluation(returns: Vec<ReturnPoint>, folds: Vec<FoldReport>) -> Result<Evaluation, Reason> {
    let (mut capital, mut peak, mut dd, mut lo, mut hi) = (1.0_f64, 1.0_f64, 0.0_f64, 1.0_f64, 1.0_f64);
    let mut equity = Vec::new();
    let mut bound = 1.0_f64;
    for f in &folds {
        for m in [&f.train, &f.oos].into_iter().filter_map(Estimate::value) {
            bound = bound.max(m.sharpe.abs() * YEAR.sqrt());
        }
    }
    let mut previous_date = None;
    for p in &returns {
        if !p.net_return.is_finite() || p.net_return <= -1. {
            return Err(Reason::Ruin);
        }
        capital *= 1. + p.net_return;
        if !capital.is_finite() || capital <= 0. {
            return Err(Reason::InvalidNumeric);
        }
        peak = peak.max(capital);
        dd = dd.min(capital / peak - 1.);
        lo = lo.min(capital);
        hi = hi.max(capital);
        let fold = folds.iter().position(|f| p.period_start >= f.test.start && p.period_end <= f.test.end).unwrap_or(0);
        let current_date = chrono::NaiveDate::parse_from_str(&p.session_date, "%Y-%m-%d").ok();
        let date_gap = previous_date
            .zip(current_date)
            .is_some_and(|(previous, current)| crate::history::calendar::next_session(previous) != current);
        let break_before = date_gap || equity.last().is_none_or(|last: &EquityPoint| last.fold != fold);
        previous_date = current_date;
        equity.push(EquityPoint { ts: p.period_end, equity: capital, fold, break_before });
    }
    // Keep min/max envelopes per fold bucket, not a smoothed fabricated path.
    if equity.len() > 2048 {
        let step = equity.len().div_ceil(1024);
        equity = equity
            .chunks(step)
            .flat_map(|chunk| {
                let mut indices = vec![0, chunk.len() - 1];
                indices.push(chunk.iter().enumerate().min_by(|a, b| a.1.equity.total_cmp(&b.1.equity)).unwrap().0);
                indices.push(chunk.iter().enumerate().max_by(|a, b| a.1.equity.total_cmp(&b.1.equity)).unwrap().0);
                indices.extend(
                    chunk
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| p.break_before)
                        .flat_map(|(i, _)| [i.saturating_sub(1), i]),
                );
                indices.sort_unstable();
                indices.dedup();
                indices.into_iter().map(|i| chunk[i].clone()).collect::<Vec<_>>()
            })
            .collect();
    }
    let mut weighted_is = 0.;
    let mut weighted_oos = 0.;
    let mut total = 0;
    let mut aggregate = Ok(());
    for f in &folds {
        match (f.train.value(), f.oos.value(), f.retention.value()) {
            (Some(a), Some(b), Some(_)) => {
                weighted_is += a.sharpe * f.test_n as f64;
                weighted_oos += b.sharpe * f.test_n as f64;
                total += f.test_n;
            }
            _ => aggregate = Err(f.retention.reason().cloned().unwrap_or(Reason::TooFewReturns)),
        }
    }
    let retention = if total == 0 {
        Estimate::Unavailable(Reason::TooFewReturns)
    } else {
        aggregate.map(|_| weighted_oos / weighted_is).into()
    };
    let full_oos = moments(&returns.iter().map(|p| p.net_return).collect::<Vec<_>>()).into();
    Ok(Evaluation {
        returns,
        folds,
        equity,
        equity_bounds: [lo, hi],
        scatter_bound: bound * 1.1,
        max_drawdown: dd,
        full_oos,
        retention,
    })
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    pub moments: Moments,
    /// PSR(0): probability the true Sharpe > 0, with no deflation.
    pub psr: Estimate<f64>,
    /// PSR(SR*): probability the true Sharpe beats SR* (`null_maximum`), the
    /// expected best Sharpe of N_eff zero-skill trials. Not P(true Sharpe > 0).
    pub dsr: Estimate<f64>,
    /// SR*, per session: √V[SR_k] × a_N(N_eff).
    pub null_maximum: f64,
    pub sharpe_above_null: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FamilyStatistics {
    pub dates: Vec<String>,
    pub stochastic_trials: usize,
    pub excluded_zero_trials: usize,
    pub cluster_sizes: Vec<usize>,
    pub effective_trials: usize,
    pub sharpe_dispersion: f64,
    pub comparisons: BTreeMap<String, Estimate<Comparison>>,
}
/// Intersection of actual evaluation dates, never union or zero-fill.
pub fn compare_family(tapes: &BTreeMap<String, Result<Vec<ReturnPoint>, Reason>>) -> Result<FamilyStatistics, Reason> {
    if tapes.is_empty() {
        return Err(Reason::RegisterUnavailable);
    }
    if tapes.len() > MAX_TRIALS {
        return Err(Reason::WorkBudgetExceeded { trials: tapes.len() });
    }
    let mut dates: Option<BTreeSet<String>> = None;
    for tape in tapes.values() {
        let tape = tape.as_ref().map_err(|_| Reason::IncompleteFamily)?;
        let ds: BTreeSet<_> = tape.iter().map(|p| p.session_date.clone()).collect();
        if ds.len() != tape.len() || tape.iter().any(|p| !p.net_return.is_finite()) {
            return Err(Reason::IncompleteFamily);
        }
        dates = Some(match dates {
            None => ds,
            Some(old) => old.intersection(&ds).cloned().collect(),
        });
    }
    let dates: Vec<_> = dates.unwrap_or_default().into_iter().collect();
    if dates.len() < 4 {
        return Err(Reason::NoComparableTrialWindow);
    }
    let mut columns = Vec::new();
    let mut moments_by_id = BTreeMap::new();
    let mut zeros = 0;
    for (id, tape) in tapes {
        let map: BTreeMap<_, _> =
            tape.as_ref().unwrap().iter().map(|p| (p.session_date.as_str(), p.net_return)).collect();
        let col: Vec<_> = dates.iter().map(|d| map[d.as_str()]).collect();
        if col.iter().all(|v| *v == 0.) {
            zeros += 1;
            moments_by_id.insert(id.clone(), Estimate::Unavailable(Reason::ZeroVariance));
            continue;
        }
        let m = moments(&col).map_err(|_| Reason::IncompleteFamily)?;
        moments_by_id.insert(id.clone(), Estimate::Available(m));
        columns.push(col);
    }
    if columns.is_empty() {
        return Err(Reason::ZeroVariance);
    }
    let groups = clusters(&columns)?;
    let ss: Vec<_> = moments_by_id.values().filter_map(Estimate::value).map(|m| m.sharpe).collect();
    let mean = ss.iter().sum::<f64>() / ss.len() as f64;
    let dispersion = if ss.len() < 2 {
        0.
    } else {
        (ss.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / (ss.len() - 1) as f64).sqrt()
    };
    let null = expected_maximum(groups.len(), dispersion)?;
    let comparisons = moments_by_id
        .into_iter()
        .map(|(id, m)| {
            let c = match m {
                Estimate::Unavailable(r) => Estimate::Unavailable(r),
                Estimate::Available(m) => Estimate::Available(Comparison {
                    moments: m,
                    psr: psr(&m, 0.).into(),
                    dsr: psr(&m, null).into(),
                    null_maximum: null,
                    sharpe_above_null: m.sharpe - null,
                }),
            };
            (id, c)
        })
        .collect();
    Ok(FamilyStatistics {
        dates,
        stochastic_trials: ss.len(),
        excluded_zero_trials: zeros,
        cluster_sizes: groups.iter().map(Vec::len).collect(),
        effective_trials: groups.len(),
        sharpe_dispersion: dispersion,
        comparisons,
    })
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariantReport {
    pub id: String,
    pub label: String,
    pub benchmark: bool,
    pub evaluation: Estimate<Evaluation>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HonestReport {
    pub methodology_version: String,
    pub settings: HonestySettings,
    pub trial_family: Estimate<crate::backtest_trials::FamilySnapshot>,
    pub family_statistics: Estimate<FamilyStatistics>,
    pub variants: Vec<VariantReport>,
    pub closed_through: i64,
    pub admission: String,
    pub execution: String,
    pub replay_statuses: BTreeMap<String, String>,
}
/// Conservative daily gap policy: do not pretend to know overnight holding P&L
/// through an absent session. Holidays use the archive calendar, not weekdays.
pub fn validate_daily(bars: &[Bar]) -> Result<(), Reason> {
    for (i, b) in bars.iter().enumerate() {
        let Some(date) = chrono::DateTime::from_timestamp(b.ts, 0).map(|d| d.date_naive()) else {
            return Err(Reason::DailyGap);
        };
        if !crate::history::calendar::is_session(date)
            || ![b.open, b.close, b.high, b.low].iter().all(|x| x.is_finite() && *x > 0.)
            || b.high < b.open.max(b.close)
            || b.low > b.open.min(b.close)
        {
            return Err(Reason::DailyGap);
        }
        if i > 0 {
            let prev = chrono::DateTime::from_timestamp(bars[i - 1].ts, 0).unwrap().date_naive();
            if date != crate::history::calendar::next_session(prev) {
                return Err(Reason::DailyGap);
            }
        }
    }
    Ok(())
}

pub fn evaluate_protocol(
    units: &[ReturnPoint],
    warmup: usize,
    intraday: bool,
    protocol: &crate::backtest_trials::Protocol,
    mut simulate: impl FnMut(usize, usize, bool) -> Result<Tape, Reason>,
) -> Result<Evaluation, Reason> {
    match protocol {
        crate::backtest_trials::Protocol::Holdout { fraction } => {
            let start = if intraday {
                crate::intraday_backtest::holdout_split(units.len(), *fraction)
            } else {
                crate::backtest::holdout_split(units.len(), *fraction)
            }
            .map_err(Reason::ReplayFailed)?;
            evaluation(simulate(start, units.len(), false)?.returns, Vec::new())
        }
        crate::backtest_trials::Protocol::WalkForward(settings) => {
            let schedule = schedule(units.len(), warmup, intraday, *settings)?;
            let mut folds = Vec::new();
            let mut returns = Vec::new();
            let mut embargoes = Vec::<Span>::new();
            for f in schedule {
                let train_tape = simulate(0, f.train.end, false)?;
                if train_tape.returns.len() != f.train.end {
                    return Err(Reason::InvalidNumeric);
                }
                let test = span(units, f.test);
                let embargo_intervals: Vec<_> = embargoes.iter().map(|s| (s.start, s.end)).collect();
                let (keep, removed_labels) = purge(&train_tape, f.train, (test.start, test.end), &embargo_intervals);
                let purged_labels: Vec<_> = train_tape
                    .labels
                    .iter()
                    .filter(|l| {
                        label_touches_candidate(l, f.train)
                            && label_is_purged(l, (test.start, test.end), &embargo_intervals)
                    })
                    .cloned()
                    .collect();
                let train_returns: Vec<_> =
                    train_tape.returns.iter().zip(&keep).filter(|(_, k)| **k).map(|(p, _)| p.net_return).collect();
                let train_n = train_returns.len();
                let mut train: Estimate<Moments> = moments(&train_returns).into();
                if train_n < 2 && purged_labels.iter().any(|l| l.exit_ts.is_none()) {
                    train = Estimate::Unavailable(Reason::UnresolvedHoldingLabel);
                }
                let test_tape = simulate(f.test.start, f.test.end, true)?;
                if test_tape.returns.len() != f.test.end - f.test.start {
                    return Err(Reason::InvalidNumeric);
                }
                let oos: Estimate<Moments> =
                    moments(&test_tape.returns.iter().map(|p| p.net_return).collect::<Vec<_>>()).into();
                let retention = retention(&train, &oos);
                let net_return = test_tape.returns.iter().fold(1., |c, p| c * (1. + p.net_return)) - 1.;
                let embargo = (f.embargo.end > f.embargo.start).then(|| span(units, f.embargo));
                folds.push(FoldReport {
                    train_candidate: span(units, f.train),
                    train_used: mask_spans(units, &keep, true, f.train),
                    purged: mask_spans(units, &keep, false, f.train),
                    prior_embargoes: embargoes.clone(),
                    test,
                    embargo: embargo.clone(),
                    nominal_embargo_end: f.nominal_embargo_end,
                    removed_labels,
                    purged_labels,
                    train_n,
                    test_n: test_tape.returns.len(),
                    train,
                    oos,
                    retention,
                    net_return,
                });
                returns.extend(test_tape.returns);
                if let Some(e) = embargo {
                    embargoes.push(e);
                }
            }
            evaluation(returns, folds)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backtest::{self, CostModel, StrategySpec},
        backtest_trials::Protocol,
    };
    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-11, "{a:.16} != {b:.16}");
    }
    fn point(i: usize, r: f64) -> ReturnPoint {
        ReturnPoint {
            session_date: format!("2026-{i:04}"),
            period_start: i as i64 * 10,
            period_end: i as i64 * 10 + 9,
            net_return: r,
        }
    }
    pub(super) fn daily(n: usize) -> Vec<Bar> {
        let mut date = chrono::NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
        let mut bars = Vec::new();
        while bars.len() < n {
            if crate::history::calendar::is_session(date) {
                let i = bars.len() as f64;
                let p = 100. + i * 0.08 + (i * 0.67).sin() * 3.;
                bars.push(
                    Bar::new(
                        date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(),
                        p,
                        p + 1.,
                        p - 1.,
                        p + 0.3 * (i * 0.41).cos(),
                        1000.,
                    )
                    .with_provenance(tr_core::Provenance::Real),
                );
            }
            date += chrono::Duration::days(1);
        }
        bars
    }
    #[test]
    fn backtest_honesty_hand_computed_moments_psr_dsr() {
        let m = moments(&[0., 0.01, 0.01, 0.02]).unwrap();
        near(m.mean, 0.01);
        near(m.sample_sd.powi(2), 0.0002 / 3.);
        near(m.sharpe, (1.5_f64).sqrt());
        near(m.skew, 0.);
        near(m.pearson_kurtosis, 2.);
        near(psr_variance(&m), 11. / 8.);
        near(psr(&m, 0.).unwrap(), 0.964_779_785_363_955_9);
        let null = expected_maximum(2, 0.1).unwrap();
        near(null, 0.056_418_958_354_775_64);
        near(psr(&m, null).unwrap(), 0.957_802_157_730_193_1);
        near(expected_maximum(1, 0.1).unwrap(), 0.);
        near(psr(&m, expected_maximum(1, 0.1).unwrap()).unwrap(), psr(&m, 0.).unwrap());
        assert_ne!(psr(&Moments { pearson_kurtosis: m.pearson_kurtosis - 3., ..m }, 0.), psr(&m, 0.));
    }
    #[test]
    fn backtest_honesty_symmetric_skew_and_annual_units() {
        let symmetric = moments(&[-0.02, -0.01, 0.01, 0.02]).unwrap();
        near(psr(&symmetric, 0.).unwrap(), 0.5);
        let m = moments(&[-0.01, 0., 0.01, 0.08]).unwrap();
        assert!(m.skew > 0.);
        let opposite = Moments { skew: -m.skew, ..m };
        assert!(psr_variance(&m) < psr_variance(&opposite));
        let scaled = moments(&[-1., 0., 1., 8.]).unwrap();
        near(m.sharpe, scaled.sharpe);
        near(psr(&m, 0.).unwrap(), psr(&scaled, 0.).unwrap());
        assert!(m.sharpe * YEAR.sqrt() > m.sharpe);
        near(m.skew, scaled.skew);
        assert_eq!(psr(&Moments { n: 3, ..m }, 0.), Err(Reason::TooFewReturns));
        assert_eq!(psr(&Moments { skew: 100., ..m }, 0.), Err(Reason::InvalidProbabilityVariance));
    }
    #[test]
    fn backtest_honesty_numerical_limits_and_normal_tails() {
        assert_eq!(moments(&[1.; 4]), Err(Reason::ZeroVariance));
        assert_eq!(moments(&[1., f64::NAN, 2., 3.]), Err(Reason::InvalidNumeric));
        assert_eq!(moments(&[1., f64::INFINITY, 2., 3.]), Err(Reason::InvalidNumeric));
        assert!(moments(&[-f64::MAX, f64::MAX, -f64::MAX, f64::MAX]).is_err());
        let large = moments(&[1e12, 1e12 + 1., 1e12 + 2., 1e12 + 3.]).unwrap();
        near(large.sample_sd, (5_f64 / 3.).sqrt());
        near(tr_core::options::norm_cdf(0.), 0.5);
        near(tr_core::options::norm_cdf(1.959963984540054), 0.975);
        near(tr_core::options::norm_cdf(-40.), 0.);
        near(tr_core::options::norm_cdf(40.), 1.);
        assert!((crate::studio_dist::normal_quantile(0.975) - 1.959963984540054).abs() < 1e-7);
        // Independent standard-library NormalDist inverse-CDF calculation.
        assert!((expected_maximum(10, 1.).unwrap() - 1.57459830134575).abs() < 1e-7);
        assert!(expected_maximum(100, 1.).unwrap() > expected_maximum(10, 1.).unwrap());
    }
    #[test]
    fn backtest_honesty_clusters_identical_orthogonal_opposite_and_chain() {
        let x = vec![-1., -1., 1., 1.];
        let y = vec![-1., 1., -1., 1.];
        assert_eq!(clusters(&[x.clone(), x.clone()]).unwrap(), vec![vec![0, 1]]);
        assert_eq!(clusters(&[x.clone(), y.clone()]).unwrap().len(), 2);
        assert_eq!(clusters(&[x.clone(), x.iter().map(|v| -v).collect()]).unwrap().len(), 2);
        // A=0°, B=20°, C=40°. AB=BC>.90, AC<.90: no chain merge.
        let angle = 20_f64.to_radians();
        let rotated = |theta: f64| x.iter().zip(&y).map(|(a, b)| theta.cos() * a + theta.sin() * b).collect::<Vec<_>>();
        let groups = clusters(&[x.clone(), rotated(angle), rotated(2. * angle)]).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups.iter().map(Vec::len).max(), Some(2));
    }
    #[test]
    fn backtest_honesty_common_dates_no_zero_fill_and_family_failures() {
        let mut tapes = BTreeMap::new();
        let xs = [-0.01, 0., 0.01, 0.02, 0.03, 0.04];
        tapes.insert("b".into(), Ok(xs.iter().enumerate().map(|(i, r)| point(i, *r)).collect()));
        tapes.insert("a".into(), Ok(xs.iter().enumerate().skip(2).map(|(i, r)| point(i, *r)).collect()));
        let family = compare_family(&tapes).unwrap();
        assert_eq!(family.dates.len(), 4);
        assert_eq!(family.effective_trials, 1);
        near(family.sharpe_dispersion, 0.);
        let cmp = family.comparisons["a"].value().unwrap();
        near(*cmp.psr.value().unwrap(), *cmp.dsr.value().unwrap());
        tapes.insert("zero".into(), Ok((0..6).map(|i| point(i, 0.)).collect()));
        assert_eq!(compare_family(&tapes).unwrap().excluded_zero_trials, 1);
        tapes.insert("constant".into(), Ok((0..6).map(|i| point(i, 0.01)).collect()));
        assert_eq!(compare_family(&tapes), Err(Reason::IncompleteFamily));
        tapes.remove("constant");
        tapes.insert("failed".into(), Err(Reason::Ruin));
        assert_eq!(compare_family(&tapes), Err(Reason::IncompleteFamily));
        tapes.remove("failed");
        tapes.insert("late".into(), Ok(vec![point(5, 0.01)]));
        assert_eq!(compare_family(&tapes), Err(Reason::NoComparableTrialWindow));
        let oversized = (0..65).map(|i| (i.to_string(), Ok(Vec::new()))).collect();
        assert_eq!(compare_family(&oversized), Err(Reason::WorkBudgetExceeded { trials: 65 }));
    }
    #[test]
    fn backtest_honesty_family_dsr_uses_session_dispersion_and_the_requested_trial() {
        // Independent Python statistics.NormalDist/sample-stdev calculation.
        // Deliberately reverse one tape and add non-common, extreme observations:
        // neither storage order nor data outside the common window may enter DSR.
        let x = [0., 0.01, 0.02, 0.04, -0.01, 0.03];
        let y = [0.03, -0.01, 0.01, 0.02, 0., -0.02];
        let mut a: Vec<_> = x.iter().enumerate().map(|(i, r)| point(i, *r)).collect();
        a.push(point(100, 0.9));
        let b = y.iter().enumerate().rev().map(|(i, r)| point(i, *r)).collect();
        let family = compare_family(&BTreeMap::from([("a".into(), Ok(a)), ("b".into(), Ok(b))])).unwrap();
        assert_eq!(family.dates.len(), 6);
        assert_eq!(family.effective_trials, 2);
        near(family.sharpe_dispersion, 0.37796447300922714);
        let a = family.comparisons["a"].value().unwrap();
        let b = family.comparisons["b"].value().unwrap();
        near(a.null_maximum, 0.21324361862292304);
        near(b.null_maximum, a.null_maximum);
        near(a.moments.sharpe, 0.8017837257372731);
        near(*a.psr.value().unwrap(), 0.9550506667203803);
        near(*a.dsr.value().unwrap(), 0.8934120137777781);
        near(*b.dsr.value().unwrap(), 0.5477607660965832);
        assert!(a.dsr != b.dsr); // A selected ID never inherits another trial's DSR.
    }
    #[test]
    fn backtest_honesty_independent_equal_sharpes_have_zero_null_dispersion() {
        let a = [-0.01, -0.01, 0.03, 0.03];
        let b = [-0.01, 0.03, -0.01, 0.03];
        let tapes = [a, b]
            .into_iter()
            .enumerate()
            .map(|(j, xs)| (j.to_string(), Ok(xs.iter().enumerate().map(|(i, r)| point(i, *r)).collect())))
            .collect();
        let family = compare_family(&tapes).unwrap();
        assert_eq!(family.effective_trials, 2);
        near(family.sharpe_dispersion, 0.);
        for c in family.comparisons.values().filter_map(Estimate::value) {
            near(c.null_maximum, 0.);
            near(*c.dsr.value().unwrap(), *c.psr.value().unwrap());
        }
    }
    #[test]
    fn backtest_honesty_fold_spans_match_hand_allocation() {
        let f = schedule(125, 1, false, HonestySettings::default()).unwrap();
        assert_eq!(
            f.iter().map(|f| f.test).collect::<Vec<_>>(),
            vec![
                IndexSpan { start: 60, end: 81 },
                IndexSpan { start: 82, end: 103 },
                IndexSpan { start: 104, end: 125 }
            ]
        );
        assert_eq!(f[2].embargo, IndexSpan { start: 125, end: 125 });
        assert_eq!(f[2].nominal_embargo_end, 126);
        assert_eq!(
            schedule(121, 1, false, HonestySettings::default()),
            Err(Reason::InsufficientHistory { available: 121, required: 122 })
        );
        assert!(schedule(1000, 1, false, HonestySettings { folds: 7, embargo_sessions: 1 }).is_err());
        let f = schedule(126, 1, false, HonestySettings::default()).unwrap();
        assert_eq!(f[0].test.end, 82);
    }
    #[test]
    fn backtest_honesty_purge_inclusive_censored_reversal_and_embargo() {
        let tape = Tape {
            returns: (0..8).map(|i| point(i, 0.)).collect(),
            labels: vec![
                HoldingLabel { decision_ts: 1, entry_ts: 10, exit_ts: Some(20), first_return: 1, last_return: 2 },
                HoldingLabel { decision_ts: 20, entry_ts: 30, exit_ts: None, first_return: 2, last_return: 7 },
            ],
        };
        let (keep, n) = purge(&tape, IndexSpan { start: 0, end: 8 }, (80, 99), &[]);
        assert_eq!(n, 1);
        assert_eq!(keep, vec![true, true, false, false, false, false, false, false]);
        let (keep, _) = purge(&tape, IndexSpan { start: 0, end: 8 }, (20, 30), &[]);
        assert!(!keep[1]); // exact exit touch
        let (keep, _) = purge(&tape, IndexSpan { start: 0, end: 8 }, (80, 99), &[(10, 19)]);
        assert!(!keep[1]);
    }
    #[test]
    fn backtest_honesty_injected_future_label_cannot_enter_training_statistics() {
        let units: Vec<_> = (0..40).map(|i| point(i, (i as f64 % 4. - 1.) / 100.)).collect();
        let protocol = Protocol::WalkForward(HonestySettings { folds: 2, embargo_sessions: 1 });
        let e = evaluate_protocol(&units, 0, true, &protocol, |start, end, liquidate| {
            let returns = units[start..end].to_vec();
            let labels = if liquidate {
                Vec::new()
            } else {
                assert_eq!(start, 0); // strictly forward training prefix
                vec![HoldingLabel {
                    decision_ts: units[4].period_start,
                    entry_ts: units[4].period_start,
                    // Deliberate lookahead: this training label resolves in test.
                    exit_ts: Some(units[end + 1].period_end),
                    first_return: 4,
                    last_return: end - 1,
                }]
            };
            Ok(Tape { returns, labels })
        })
        .unwrap();
        for f in &e.folds {
            assert_eq!(f.train_n, 4);
            assert_eq!(f.removed_labels, 1);
            assert_eq!(f.train_used.len(), 1);
            assert_eq!(f.train_used[0].indices, IndexSpan { start: 0, end: 4 });
            near(f.train.value().unwrap().mean, 0.005);
            assert!(f.train_used.iter().all(|s| s.end < f.test.start));
        }
    }
    #[test]
    fn backtest_honesty_purge_diagnostics_only_count_scored_labels_and_censored_exits() {
        let tape = Tape {
            returns: (0..8).map(|i| point(i, 0.)).collect(),
            labels: vec![HoldingLabel {
                decision_ts: 0,
                entry_ts: 10,
                exit_ts: Some(20),
                first_return: 1,
                last_return: 2,
            }],
        };
        let (_, count) = purge(&tape, IndexSpan { start: 4, end: 8 }, (80, 99), &[(10, 19)]);
        assert_eq!(count, 0); // An unscored warm-up label removed no training evidence.

        let units: Vec<_> = (0..40).map(|i| point(i, (i as f64 % 4. - 1.) / 100.)).collect();
        let protocol = Protocol::WalkForward(HonestySettings { folds: 2, embargo_sessions: 1 });
        for censored in [false, true] {
            let e = evaluate_protocol(&units, 0, true, &protocol, |start, end, liquidate| {
                Ok(Tape {
                    returns: units[start..end].to_vec(),
                    labels: if liquidate {
                        vec![]
                    } else {
                        vec![HoldingLabel {
                            decision_ts: 0,
                            entry_ts: 10,
                            exit_ts: if censored { None } else { Some(units[end].period_end) },
                            first_return: 0,
                            last_return: end - 1,
                        }]
                    },
                })
            })
            .unwrap();
            for f in e.folds {
                assert_eq!(f.train_n, 0);
                assert_eq!(
                    f.train,
                    Estimate::Unavailable(if censored {
                        Reason::UnresolvedHoldingLabel
                    } else {
                        Reason::TooFewReturns
                    })
                );
            }
        }
    }
    #[test]
    fn backtest_honesty_daily_liquidation_purge_stitch_and_causality() {
        let bars = daily(150);
        let free = CostModel { commission_bps: 0., slippage_bps: 0. };
        let wf = Protocol::WalkForward(HonestySettings::default());
        let bh = backtest::honest_evaluation(&bars, &StrategySpec::BuyAndHold, free, false, &wf).unwrap();
        assert_eq!(bh.folds.len(), 3);
        assert!(bh.folds.iter().all(|f| f.train_n == 0 && f.removed_labels == 1));
        assert!(bh.folds.iter().all(|f| matches!(f.train, Estimate::Unavailable(Reason::UnresolvedHoldingLabel))));
        let paid = backtest::honest_evaluation(
            &bars,
            &StrategySpec::BuyAndHold,
            CostModel { commission_bps: 100., slippage_bps: 0. },
            false,
            &wf,
        )
        .unwrap();
        for f in &paid.folds {
            let a = f.test.indices.start;
            let b = f.test.indices.end;
            near(f.net_return, bars[b - 1].close * 0.99 / (bars[a].open * 1.01) - 1.);
        }
        let product = paid.returns.iter().fold(1., |c, p| c * (1. + p.net_return));
        near(product, paid.equity.last().unwrap().equity);
        let dates: BTreeSet<_> = paid.returns.iter().map(|p| &p.session_date).collect();
        assert_eq!(dates.len(), paid.returns.len());
        for f in &paid.folds {
            if let Some(e) = &f.embargo {
                assert!(!dates.contains(&e.first_date));
            }
        }
        let mut future = bars.clone();
        let boundary = bh.folds[1].test.indices.start;
        for b in &mut future[boundary..] {
            b.open *= 1.3;
            b.close *= 1.3;
            b.high *= 1.3;
            b.low *= 1.3;
        }
        let again = backtest::honest_evaluation(&future, &StrategySpec::BuyAndHold, free, false, &wf).unwrap();
        assert_eq!(bh.folds[0], again.folds[0]);
        // Adaptive indicators are prefix-stable too, including IS inputs.
        let spec = StrategySpec::AdaptiveTrend { long_only: true, atr_n: 20, confirm: 0.25, method: 0. };
        let a = backtest::honest_evaluation(&bars, &spec, free, false, &wf).unwrap();
        let b = backtest::honest_evaluation(&future, &spec, free, false, &wf).unwrap();
        assert_eq!(a.folds[0], b.folds[0]);
    }
    #[test]
    fn backtest_honesty_daily_gap_uses_calendar_and_real_gate() {
        let bars = daily(150);
        assert!(validate_daily(&bars).is_ok());
        let mut hole = bars.clone();
        hole.remove(25);
        assert_eq!(validate_daily(&hole), Err(Reason::DailyGap));
        let mut duplicate = bars.clone();
        duplicate[25] = duplicate[24];
        assert_eq!(validate_daily(&duplicate), Err(Reason::DailyGap));
        let mut bad = bars;
        bad[0].provenance = tr_core::Provenance::Sample;
        assert!(
            backtest::honest_evaluation(
                &bad,
                &StrategySpec::BuyAndHold,
                CostModel::default(),
                false,
                &Protocol::WalkForward(Default::default())
            )
            .is_err()
        );
    }
    #[test]
    fn backtest_honesty_exports_round_trip_and_old_reports_default() {
        let bars = daily(160);
        let settings = backtest::RunSettings::default().with_strategies(vec![StrategySpec::BuyAndHold]);
        let wf = HonestySettings::default();
        let experiments = crate::backtest_trials::daily_experiments(&settings, wf);
        let mut report = backtest::compute_report("AAPL US Equity", "AAPL", &bars, &settings, "fixture").unwrap();
        report.honesty = Some(crate::backtest_trials::compute(
            &bars,
            tr_core::Timeframe::D1,
            wf,
            &experiments,
            Estimate::Unavailable(Reason::RegisterUnavailable),
            "verified test fixture".into(),
        ));
        let doc = crate::portable::backtest_document(&report, "2026-11-14").unwrap();
        assert_eq!(doc.payload["honesty"]["methodology_version"], METHOD);
        assert!(doc.payload["honesty"]["variants"][0]["evaluation"]["Available"]["folds"].is_array());
        let decoded: backtest::BacktestReport = serde_json::from_value(doc.payload.clone()).unwrap();
        assert_eq!(decoded, report);
        let mut old = doc.payload;
        old.as_object_mut().unwrap().remove("honesty");
        assert!(serde_json::from_value::<backtest::BacktestReport>(old).unwrap().honesty.is_none());
    }
    #[test]
    fn backtest_honesty_equity_breaks_across_missing_sessions() {
        let mut a = point(0, 0.01);
        a.session_date = "2026-11-09".into();
        let mut b = point(1, -0.01);
        b.session_date = "2026-11-11".into();
        let e = evaluation(vec![a, b], vec![]).unwrap();
        assert!(e.equity[1].break_before);
    }
    #[test]
    fn backtest_honesty_retention_and_ruin_refuse_fake_results() {
        let m = moments(&[0., 0.01, 0.01, 0.02]).unwrap();
        let a = Estimate::Available(m);
        let b = Estimate::Available(Moments { sharpe: -m.sharpe, ..m });
        near(*retention(&a, &b).value().unwrap(), -1.);
        assert!(retention(&b, &a).value().is_none());
        assert_eq!(evaluation(vec![point(0, -1.)], vec![]), Err(Reason::Ruin));
        assert_eq!(evaluation(vec![point(0, f64::NAN)], vec![]), Err(Reason::Ruin));
    }
}
