//! Regression Lab (Studio slice 5): multi-factor OLS on verified real data with per-factor
//! transforms and lags on a strict grid, a pivoted-QR fit with rank/κ guards, four covariance
//! estimates, a chronological *purged* holdout with frozen coefficients, and bounded diagnostics.
//!
//! Conventions
//! * The grid is the **target's own bars**. A factor value at target bar `t` with lag ℓ is the
//!   factor transform evaluated at target bar `t−ℓ`; it is never later than `t`.
//! * Rows are complete cases only. Nothing is imputed, interpolated or carried; every missing cell
//!   is counted by reason in [`Design::dropped`].
//! * Fit, standardisation and the baseline mean use the **training** rows only. Coefficients are then
//!   frozen and scored on the test rows (Campbell–Thompson out-of-sample R²).
//! * Purge only: training rows whose target support `(g−h_y, g]` overlaps the first test target's
//!   support are dropped (at most `h_y−1` rows). There is no test-side embargo.
use crate::nulls::Family;
use crate::stable_hash::{Fnv64, bars_digest};
use crate::studio::{SeriesReceipt, SeriesUnit, Transform};
use crate::studio_corr::NamedBars;
use crate::studio_corr::SE_METHODS;
use crate::studio_corr::relate::{MAX_POINTS, effective_method, prefix, union_gaps, unit_for};
use crate::studio_inference::{CovarianceMethod, RegressionCovariance, normal_p, regression_covariance};
use crate::studio_linalg::{self as linalg, LinalgError};
use serde::{Deserialize, Serialize};
use tr_core::{Bar, Provenance, Timeframe};

pub const METHOD_SCHEMA: &str = "lab.v1";
pub const MAX_FACTORS: usize = 7;
pub const MAX_LAG: usize = 250;
pub const MIN_TEST: usize = 20;
pub const KAPPA_MAX: f64 = linalg::KAPPA_MAX;
pub const RANK_TOL: f64 = linalg::RANK_TOL;
/// Histogram bins (width 0.2 over ±4.1σ) and residual-vs-fitted grid dimensions.
pub const HIST_BINS: usize = 41;
pub const GRID_DIMS: (usize, usize) = (48, 24);
/// Method summary keyed into every report.
pub const MODEL_NOTE: &str = "lab.v1 qr-pivot kappa<=1e6 purge=h_y-1 se=all4";
/// Required training rows for inference: `max(60, 10·k)` with `k = p + 1` (intercept).
pub fn min_train(k: usize) -> usize {
    60.max(10 * k)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactorSpec {
    /// Security key (`TLT US Equity`).
    pub symbol: String,
    pub transform: Transform,
    pub lag: usize,
}
impl FactorSpec {
    pub fn new(symbol: &str, transform: Transform, lag: usize) -> Self {
        Self { symbol: crate::security(symbol).key(), transform, lag }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LabSpec {
    pub target: Transform,
    /// Coefficient order == spec order.
    pub factors: Vec<FactorSpec>,
    /// Holdout share in percent, 10..=40.
    pub holdout_pct: u8,
    /// Display default only; the report always holds all four estimators. Not in the key.
    pub covariance: CovarianceMethod,
}
impl Default for LabSpec {
    fn default() -> Self {
        Self {
            target: Transform::LogReturn { h: 1 },
            factors: Vec::new(),
            holdout_pct: 20,
            covariance: CovarianceMethod::HacAutomatic,
        }
    }
}

fn transform_name(t: Transform) -> &'static str {
    match t {
        Transform::Level => "level",
        Transform::LogLevel => "loglevel",
        Transform::Change { .. } => "change",
        Transform::LogReturn { .. } => "logreturn",
    }
}
fn covariance_name(m: CovarianceMethod) -> &'static str {
    match m {
        CovarianceMethod::Classical => "classical",
        CovarianceMethod::HC3 => "hc3",
        CovarianceMethod::HacFixed5 => "hac5",
        _ => "hac-auto",
    }
}
/// Transform after `Transform::parse`, with the Lab's own error wording.
fn parse_transform(name: &str, h: usize) -> Result<Transform, String> {
    if ![1, 5, 20].contains(&h) {
        return Err("Lab h must be 1, 5 or 20".into());
    }
    Transform::parse(name, h).map_err(|e| if e.starts_with("h must") { format!("Lab {e}") } else { e })
}

impl LabSpec {
    /// The suggested single factor: SPY, or QQQ when the target is SPY.
    pub fn default_for(symbol: &str) -> Self {
        let spy = crate::security("SPY").key();
        let factor = if crate::security(symbol).key() == spy { "QQQ" } else { "SPY" };
        Self { factors: vec![FactorSpec::new(factor, Transform::LogReturn { h: 1 }, 0)], ..Default::default() }
    }
    /// Count, duplicate, lag, horizon and holdout rules; does not know the target symbol.
    pub fn validate(&self) -> Result<(), String> {
        if self.factors.is_empty() {
            return Err("Regression Lab needs at least one factor".into());
        }
        if self.factors.len() > MAX_FACTORS {
            return Err(format!("Regression Lab allows at most {MAX_FACTORS} factors"));
        }
        if !(10..=40).contains(&self.holdout_pct) {
            return Err("holdout must be 10–40%".into());
        }
        for t in std::iter::once(&self.target).chain(self.factors.iter().map(|f| &f.transform)) {
            if ![1, 5, 20].contains(&t.horizon()) {
                return Err("Lab h must be 1, 5 or 20".into());
            }
        }
        for (i, f) in self.factors.iter().enumerate() {
            if f.lag > MAX_LAG {
                return Err(format!("lag must be 0–{MAX_LAG}"));
            }
            if self.factors[..i].iter().any(|g| g.symbol == f.symbol) {
                return Err(format!("duplicate factor {}", crate::security(&f.symbol).ticker));
            }
        }
        Ok(())
    }
    /// `validate` plus "no factor equals the target".
    pub fn validate_for(&self, target_symbol: &str) -> Result<(), String> {
        self.validate()?;
        let key = crate::security(target_symbol).key();
        if self.factors.iter().any(|f| f.symbol == key) {
            return Err(format!("the target {} cannot also be a factor", crate::security(&key).ticker));
        }
        Ok(())
    }
    /// Cache identity: target transform, the ordered factor triples and the holdout. Excludes the
    /// displayed covariance (all four are always computed).
    pub fn key_string(&self) -> String {
        let factors: Vec<_> = self.factors.iter().map(|f| (&f.symbol, f.transform, f.lag)).collect();
        format!("target={:?} factors={factors:?} holdout={}", self.target, self.holdout_pct)
    }
    /// Command words that re-parse to this spec.
    pub fn command(&self) -> String {
        let item = |f: &FactorSpec| {
            let word = crate::security(&f.symbol).ticker;
            let word = if crate::security(&word).key() == f.symbol { word } else { f.symbol.replace(' ', "_") };
            format!("{word}:{}:{}:{}", transform_name(f.transform), f.transform.horizon(), f.lag)
        };
        let mut out = String::new();
        if !self.factors.is_empty() {
            out += &format!("factors={} ", self.factors.iter().map(item).collect::<Vec<_>>().join(","));
        }
        format!(
            "{out}target={}:{} covariance={} holdout={}%",
            transform_name(self.target),
            self.target.horizon(),
            covariance_name(self.covariance),
            self.holdout_pct
        )
    }
    /// Largest overlap horizon across target and factors. Lags do not enter.
    pub fn horizon_max(&self) -> usize {
        self.factors.iter().map(|f| f.transform.horizon()).chain([self.target.horizon()]).max().unwrap_or(1)
    }
    /// Short row label, e.g. `TLT · logret h=1 · lag 0`.
    pub fn term_label(f: &FactorSpec) -> String {
        format!("{} · {} · lag {}", crate::security(&f.symbol).ticker, f.transform.short(), f.lag)
    }
}

/// One `factor=SYM` edit group.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FactorEdit {
    pub symbol: String,
    pub transform: Option<String>,
    pub h: Option<usize>,
    pub lag: Option<usize>,
}

/// Parsed words after `lab`; see the grammar in `docs/STUDIO_LAB.md`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LabArgs {
    pub tf: Option<Timeframe>,
    pub target: Option<Transform>,
    pub factors: Option<Vec<FactorSpec>>,
    pub edits: Vec<FactorEdit>,
    pub removes: Vec<String>,
    pub covariance: Option<CovarianceMethod>,
    pub holdout: Option<u8>,
}

fn symbol_key(word: &str) -> Result<String, String> {
    if word.is_empty() || !word.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '^' | '=' | '_')) {
        return Err(format!("unknown Lab symbol `{word}`"));
    }
    Ok(crate::security(&word.replace('_', " ")).key())
}
fn parse_lag(s: &str) -> Result<usize, String> {
    match s.parse::<usize>() {
        Ok(l) if l <= MAX_LAG => Ok(l),
        _ => Err(format!("lag must be 0–{MAX_LAG}")),
    }
}
fn parse_h(s: &str) -> Result<usize, String> {
    s.parse::<usize>().ok().filter(|h| [1, 5, 20].contains(h)).ok_or_else(|| "Lab h must be 1, 5 or 20".to_string())
}

impl LabArgs {
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut out = Self::default();
        let mut group: Option<usize> = None;
        let (mut seen_t, mut seen_h, mut seen_l) = (false, false, false);
        for word in spec.split_whitespace() {
            let Some((key, value)) = word.split_once('=') else {
                if out.tf.is_some() {
                    return Err(format!("Unknown or repeated Lab input: {word}"));
                }
                let tf: Timeframe = word.parse().map_err(|_| format!("Unknown Lab input: {word}"))?;
                if tf != Timeframe::D1 && !tf.is_intraday() {
                    return Err("Lab supports daily or intraday intervals".into());
                }
                out.tf = Some(tf);
                continue;
            };
            let key = key.to_ascii_lowercase();
            let lower = value.to_ascii_lowercase();
            match key.as_str() {
                "target" => {
                    if out.target.is_some() {
                        return Err("repeated Lab input: target".into());
                    }
                    let (name, h) = lower.split_once(':').map_or((lower.as_str(), None), |(n, h)| (n, Some(h)));
                    out.target = Some(parse_transform(name, h.map_or(Ok(1), parse_h)?)?);
                }
                "factors" => {
                    if out.factors.is_some() {
                        return Err("repeated Lab input: factors".into());
                    }
                    if value.is_empty() {
                        return Err("factors= needs at least one symbol".into());
                    }
                    let mut list: Vec<FactorSpec> = Vec::new();
                    for item in value.split(',') {
                        let mut parts = item.split(':');
                        let sym = symbol_key(parts.next().unwrap_or(""))?;
                        let name = parts.next().map(str::to_ascii_lowercase);
                        let h = parts.next().map_or(Ok(1), parse_h)?;
                        let lag = parts.next().map_or(Ok(0), parse_lag)?;
                        if parts.next().is_some() {
                            return Err(format!("too many `:` parts in factor `{item}`"));
                        }
                        let transform = parse_transform(name.as_deref().unwrap_or("logreturn"), h)?;
                        if list.iter().any(|f| f.symbol == sym) {
                            return Err(format!("duplicate factor {}", crate::security(&sym).ticker));
                        }
                        list.push(FactorSpec { symbol: sym, transform, lag });
                    }
                    if list.len() > MAX_FACTORS {
                        return Err(format!("Regression Lab allows at most {MAX_FACTORS} factors"));
                    }
                    out.factors = Some(list);
                }
                "factor" => {
                    out.edits.push(FactorEdit { symbol: symbol_key(value)?, ..Default::default() });
                    group = Some(out.edits.len() - 1);
                    (seen_t, seen_h, seen_l) = (false, false, false);
                }
                "transform" | "h" | "lag" => {
                    let Some(g) = group else { return Err("use target=… for the target".into()) };
                    let edit = &mut out.edits[g];
                    let seen = match key.as_str() {
                        "transform" => &mut seen_t,
                        "h" => &mut seen_h,
                        _ => &mut seen_l,
                    };
                    if std::mem::replace(seen, true) {
                        return Err(format!("repeated Lab input: {key}"));
                    }
                    match key.as_str() {
                        "transform" => {
                            parse_transform(&lower, 1)?;
                            edit.transform = Some(lower);
                        }
                        "h" => edit.h = Some(parse_h(&lower)?),
                        _ => edit.lag = Some(parse_lag(&lower)?),
                    }
                }
                "remove" => out.removes.push(symbol_key(value)?),
                "covariance" => {
                    if out.covariance.is_some() {
                        return Err("repeated Lab input: covariance".into());
                    }
                    out.covariance = Some(match lower.as_str() {
                        "classical" => CovarianceMethod::Classical,
                        "hc3" => CovarianceMethod::HC3,
                        "hac5" => CovarianceMethod::HacFixed5,
                        "hac-auto" => CovarianceMethod::HacAutomatic,
                        _ => return Err("covariance must be classical, hc3, hac5 or hac-auto".into()),
                    });
                }
                "holdout" => {
                    if out.holdout.is_some() {
                        return Err("repeated Lab input: holdout".into());
                    }
                    let pct = lower.strip_suffix('%').unwrap_or(&lower);
                    out.holdout = Some(
                        pct.parse::<u8>().ok().filter(|p| (10..=40).contains(p)).ok_or("holdout must be 10–40%")?,
                    );
                }
                _ => return Err(format!("Unknown or repeated Lab input: {word}")),
            }
        }
        Ok(out)
    }
    /// True when the words change the model and name no interval (a focused Lab can take them as edits).
    pub fn is_edit_only(&self) -> bool {
        self.tf.is_none()
    }
    /// Applies every word to `spec`, then validates.
    pub fn apply(&self, spec: &mut LabSpec) -> Result<(), String> {
        if let Some(t) = self.target {
            spec.target = t;
        }
        if let Some(list) = &self.factors {
            spec.factors = list.clone();
        }
        for e in &self.edits {
            let at = match spec.factors.iter().position(|f| f.symbol == e.symbol) {
                Some(at) => at,
                None => {
                    spec.factors.push(FactorSpec { symbol: e.symbol.clone(), transform: Transform::LogReturn { h: 1 }, lag: 0 });
                    spec.factors.len() - 1
                }
            };
            let f = &mut spec.factors[at];
            let h = e.h.unwrap_or_else(|| f.transform.horizon());
            if let Some(name) = &e.transform {
                f.transform = parse_transform(name, h)?;
            } else if e.h.is_some() {
                f.transform = match f.transform {
                    Transform::Change { .. } => Transform::Change { h },
                    Transform::LogReturn { .. } => Transform::LogReturn { h },
                    other => other,
                };
            }
            if let Some(l) = e.lag {
                f.lag = l;
            }
        }
        for r in &self.removes {
            let before = spec.factors.len();
            spec.factors.retain(|f| &f.symbol != r);
            if spec.factors.len() == before {
                return Err(format!("no factor {} to remove", crate::security(r).ticker));
            }
        }
        if let Some(c) = self.covariance {
            spec.covariance = c;
        }
        if let Some(p) = self.holdout {
            spec.holdout_pct = p;
        }
        if spec.factors.len() > MAX_FACTORS {
            return Err(format!("Regression Lab allows at most {MAX_FACTORS} factors"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Segment {
    Train,
    Purged,
    Test,
}

/// Complete-case rows in ascending target-bar order. `x[j][row]` is column-major.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Design {
    pub ts: Vec<i64>,
    pub bar: Vec<usize>,
    pub y: Vec<f64>,
    pub x: Vec<Vec<f64>>,
    pub break_before: Vec<bool>,
    pub target_bars: usize,
    pub dropped: Vec<(String, usize)>,
    pub receipts: Vec<SeriesReceipt>,
    pub units: Vec<SeriesUnit>,
    pub grids: Vec<String>,
}

fn grid_label(symbol: &str) -> String {
    if crate::time_link::cash_class(crate::security(symbol).class) {
        "NYSE cash calendar".into()
    } else {
        "UTC grid (explicit)".into()
    }
}

fn bump(dropped: &mut Vec<(String, usize)>, reason: &str) {
    match dropped.iter_mut().find(|(r, _)| r == reason) {
        Some((_, n)) => *n += 1,
        None => dropped.push((reason.to_string(), 1)),
    }
}

/// Why a factor cell at a target bar is missing.
#[derive(Clone, Copy, PartialEq)]
enum Cell {
    Value(f64),
    MissingTimestamp,
    SpanBreak,
}

#[allow(clippy::too_many_arguments)]
pub fn build_design(
    target: &NamedBars,
    factors: &[NamedBars],
    spec: &LabSpec,
    tf: Timeframe,
    daily: bool,
    source: &str,
    closed_thru: i64,
    observed_at: Option<i64>,
) -> Result<Design, String> {
    let _ = source;
    if factors.len() != spec.factors.len() {
        return Err("factor rows do not match the spec".into());
    }
    let key = |ts: i64| if daily { ts.div_euclid(86_400) } else { ts };
    crate::stats::require_research_bars(&target.bars, "Lab target")?;
    for (f, row) in spec.factors.iter().zip(factors) {
        crate::stats::require_research_bars(&row.bars, "Lab factor")?;
        if f.symbol != row.symbol {
            return Err(format!("factor row {} does not match spec factor {}", row.symbol, f.symbol));
        }
    }
    let all: Vec<&NamedBars> = std::iter::once(target).chain(factors.iter()).collect();
    if daily && all.iter().any(|r| r.bars.windows(2).any(|w| key(w[0].ts) == key(w[1].ts))) {
        return Err("Lab refused: two daily bars share one calendar date".into());
    }
    let gaps = union_gaps(&all, daily);
    let n = target.bars.len();
    let ts: Vec<i64> = target.bars.iter().map(|b| b.ts).collect();
    let cum_t = prefix(&crate::studio_canvas::expected_breaks_for(&target.symbol, tf, &target.bars, &target.gaps));
    let tgt_ok = |a: usize, b: usize| {
        a <= b && cum_t[b + 1] == cum_t[a + 1] && !crate::studio_dist::spans_gap(&gaps, ts[a], ts[b])
    };
    let h_y = spec.target.horizon();
    let log_of = |t: Transform| matches!(t, Transform::LogReturn { .. } | Transform::LogLevel);
    // Target values.
    let mut y: Vec<Option<f64>> = vec![None; n];
    for t in 0..n {
        let p = target.bars[t].close;
        y[t] = match spec.target {
            Transform::Level => finite(p),
            Transform::LogLevel => finite(p.ln()),
            Transform::Change { .. } | Transform::LogReturn { .. } => (t >= h_y && tgt_ok(t - h_y, t))
                .then(|| {
                    let p0 = target.bars[t - h_y].close;
                    if log_of(spec.target) { finite(p.ln() - p0.ln()) } else { finite(p - p0) }
                })
                .flatten(),
        };
    }
    // Factor transform evaluated at every target bar (independent of the lag).
    let mut cells: Vec<Vec<Cell>> = Vec::with_capacity(factors.len());
    for (f, row) in spec.factors.iter().zip(factors) {
        let fk: Vec<i64> = row.bars.iter().map(|b| key(b.ts)).collect();
        // Both key lists ascend, so one merge pass maps every target bar to its factor bar.
        let mut idx: Vec<Option<usize>> = Vec::with_capacity(n);
        let mut m = 0usize;
        for t in 0..n {
            let kt = key(ts[t]);
            while m < fk.len() && fk[m] < kt {
                m += 1;
            }
            idx.push((m < fk.len() && fk[m] == kt).then_some(m));
        }
        let at = |t: usize| idx[t];
        let cum_f = prefix(&crate::studio_canvas::expected_breaks_for(&row.symbol, tf, &row.bars, &row.gaps));
        let h = f.transform.horizon();
        let mut col = vec![Cell::MissingTimestamp; n];
        for s in 0..n {
            col[s] = match f.transform {
                Transform::Level | Transform::LogLevel => match at(s) {
                    None => Cell::MissingTimestamp,
                    Some(m) => {
                        let v = row.bars[m].close;
                        finite(if f.transform == Transform::LogLevel { v.ln() } else { v })
                            .map_or(Cell::SpanBreak, Cell::Value)
                    }
                },
                Transform::Change { .. } | Transform::LogReturn { .. } => {
                    if s < h || !tgt_ok(s - h, s) {
                        Cell::SpanBreak
                    } else {
                        match (at(s - h), at(s)) {
                            (Some(m0), Some(m1)) => {
                                if m1 - m0 == h && cum_f[m1 + 1] == cum_f[m0 + 1] {
                                    let (q, q0) = (row.bars[m1].close, row.bars[m0].close);
                                    finite(if log_of(f.transform) { q.ln() - q0.ln() } else { q - q0 })
                                        .map_or(Cell::SpanBreak, Cell::Value)
                                } else {
                                    Cell::SpanBreak
                                }
                            }
                            _ => Cell::MissingTimestamp,
                        }
                    }
                }
            };
        }
        cells.push(col);
    }
    // Complete cases.
    let mut dropped: Vec<(String, usize)> = Vec::new();
    let (mut d_ts, mut d_bar, mut d_y) = (Vec::new(), Vec::new(), Vec::new());
    let mut d_x: Vec<Vec<f64>> = vec![Vec::new(); factors.len()];
    let mut vals = vec![0.; factors.len()];
    // Resolved once: `security()` parses and allocates, so it must stay out of the per-row loop.
    let names: Vec<String> = spec.factors.iter().map(|f| crate::security(&f.symbol).ticker).collect();
    for t in 0..n {
        let mut ok = true;
        let Some(yt) = y[t] else {
            bump(&mut dropped, "target span");
            continue;
        };
        for (j, f) in spec.factors.iter().enumerate() {
            let name = &names[j];
            if t < f.lag {
                bump(&mut dropped, &format!("{name} lag before start"));
                ok = false;
                continue;
            }
            let s = t - f.lag;
            if !tgt_ok(s, t) {
                bump(&mut dropped, &format!("{name} lag crosses break"));
                ok = false;
                continue;
            }
            match cells[j][s] {
                Cell::Value(v) => vals[j] = v,
                Cell::MissingTimestamp => {
                    bump(&mut dropped, &format!("{name} missing timestamp"));
                    ok = false;
                }
                Cell::SpanBreak => {
                    bump(&mut dropped, &format!("{name} span break"));
                    ok = false;
                }
            }
        }
        if ok {
            d_ts.push(ts[t]);
            d_bar.push(t);
            d_y.push(yt);
            for (col, v) in d_x.iter_mut().zip(&vals) {
                col.push(*v);
            }
        }
    }
    let break_before: Vec<bool> =
        (0..d_bar.len()).map(|i| i == 0 || d_bar[i] != d_bar[i - 1] + 1 || !tgt_ok(d_bar[i - 1], d_bar[i])).collect();
    let receipt = |r: &NamedBars| SeriesReceipt {
        source: r.source.clone(),
        provenance: Bar::provenance(&r.bars),
        observed_at,
        closed_thru,
        input_digest: bars_digest(&r.bars),
        gaps: r.gaps.clone(),
    };
    Ok(Design {
        ts: d_ts,
        bar: d_bar,
        y: d_y,
        x: d_x,
        break_before,
        target_bars: n,
        dropped,
        receipts: all.iter().map(|r| receipt(r)).collect(),
        units: std::iter::once(unit_for(spec.target))
            .chain(spec.factors.iter().map(|f| unit_for(f.transform)))
            .collect(),
        grids: all.iter().map(|r| grid_label(&r.symbol)).collect(),
    })
}

fn finite(x: f64) -> Option<f64> {
    x.is_finite().then_some(x)
}

/// Chronological split with purge. Returns (train rows, purged count, test rows, split timestamp).
/// Rows are indices into the design; train is a prefix and test a suffix.
pub fn split(d: &Design, holdout_pct: u8, h_y: usize) -> Result<(Vec<usize>, usize, Vec<usize>, i64), String> {
    let total = d.y.len();
    let n0 = total * (100 - holdout_pct as usize) / 100;
    if n0 == 0 || n0 >= total {
        return Err(format!("split unavailable: {total} complete rows cannot be divided at {}%", 100 - holdout_pct));
    }
    let s = d.bar[n0];
    let train: Vec<usize> = (0..n0).filter(|&i| d.bar[i] + h_y <= s).collect();
    let purged = n0 - train.len();
    Ok((train, purged, (n0..total).collect(), d.ts[n0]))
}

/// Campbell–Thompson out-of-sample R² against the frozen training mean.
pub fn oos_r2(y: &[f64], yhat: &[f64], train_mean: f64) -> Option<f64> {
    let sse: f64 = y.iter().zip(yhat).map(|(a, b)| (a - b) * (a - b)).sum();
    let base: f64 = y.iter().map(|a| (a - train_mean) * (a - train_mean)).sum();
    if base <= 0. || !base.is_finite() {
        return None;
    }
    finite(1. - sse / base)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoefInference {
    /// Original units; `None` covariance carries the reason.
    pub covariance: RegressionCovariance,
    pub se: Vec<Option<f64>>,
    pub interval: Vec<Option<(f64, f64)>>,
    pub z: Vec<Option<f64>>,
    /// Intercept first; the intercept is outside the family.
    pub p: Vec<Option<f64>>,
    /// Benjamini–Yekutieli over the slope p-values (unavailable members enter at p=1); `None` for
    /// the intercept.
    pub q: Vec<Option<f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FittedModel {
    pub coefficients: Vec<f64>,
    pub mean: Vec<f64>,
    pub scale: Vec<f64>,
    pub residual_sigma: f64,
    pub train_mean: f64,
    pub n_train: usize,
    pub k: usize,
    pub rank: usize,
    pub kappa_inf: f64,
    pub vif: Vec<f64>,
    pub r2_in: Option<f64>,
    pub r2_adj: Option<f64>,
    pub train_start: i64,
    pub train_end: i64,
    pub split_ts: i64,
    pub spec: LabSpec,
    pub input_identity: u64,
    pub model_id: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabHoldout {
    pub n_test: usize,
    pub purged: usize,
    pub sse: f64,
    pub baseline_sse: f64,
    pub r2_oos: Option<f64>,
    /// Every lag ≥ h_y, so each factor window ends before the target window starts.
    pub forecast_style: bool,
    pub unavailable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabDiagnostics {
    pub hist_edges: (f64, f64),
    pub hist: Vec<u32>,
    pub hist_beyond: u32,
    /// Row-major `grid_dims.1` rows (y, bottom → top) of `grid_dims.0` columns.
    pub grid: Vec<u32>,
    pub grid_dims: (usize, usize),
    pub grid_x: (f64, f64),
    pub grid_y: (f64, f64),
    pub grid_outside: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabPoint {
    pub ts: i64,
    pub y: f64,
    pub fitted: f64,
    pub residual: f64,
    pub break_before: bool,
    pub segment: Segment,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabReport {
    pub key: u64,
    pub symbol: String,
    pub tf: Timeframe,
    pub spec: LabSpec,
    pub design_rows: usize,
    pub target_bars: usize,
    pub dropped: Vec<(String, usize)>,
    pub receipts: Vec<SeriesReceipt>,
    pub grids: Vec<String>,
    pub units: Vec<SeriesUnit>,
    pub fit: Option<FittedModel>,
    /// Four entries in `SE_METHODS` order, or empty.
    pub inference: Vec<CoefInference>,
    pub holdout: Option<LabHoldout>,
    pub diagnostics: Option<LabDiagnostics>,
    pub points: Vec<LabPoint>,
    pub source: String,
    pub provenance: Provenance,
    pub closed_thru: i64,
    pub observed_at: Option<i64>,
    pub low_n: Option<String>,
    pub unavailable: Option<String>,
    pub factor_errors: Vec<(String, String)>,
    /// First and last design timestamps (the descriptive span).
    pub span: Option<(i64, i64)>,
    /// Latest ≤ `MAX_TRAIN_RESIDUALS` in-sample residuals (chronological), for saving a frozen fit.
    /// Functions of keyed content; not part of any cache key. Empty on reports from older builds.
    #[serde(default)]
    pub train_residuals: Vec<f64>,
    /// Same length as `train_residuals`; the first entry is forced `true`.
    #[serde(default)]
    pub train_breaks: Vec<bool>,
    /// Mean over **all** `n_train` training residuals (≈ 0 with an intercept).
    #[serde(default)]
    pub train_residual_mean: f64,
    /// One per input (target first): digest of each input's bars over the training span.
    #[serde(default)]
    pub train_span_digests: Vec<crate::studio_residual::SpanDigest>,
}

impl LabReport {
    /// Inference block for a display selection (index into `SE_METHODS`).
    pub fn inference_at(&self, shown: usize) -> Option<&CoefInference> {
        self.inference.get(shown)
    }
}

fn blank(
    symbol: &str,
    tf: Timeframe,
    spec: &LabSpec,
    source: &str,
    closed_thru: i64,
    observed_at: Option<i64>,
    key: u64,
    provenance: Provenance,
    reason: String,
) -> LabReport {
    LabReport {
        key,
        symbol: symbol.into(),
        tf,
        spec: spec.clone(),
        design_rows: 0,
        target_bars: 0,
        dropped: vec![],
        receipts: vec![],
        grids: vec![],
        units: vec![],
        fit: None,
        inference: vec![],
        holdout: None,
        diagnostics: None,
        points: vec![],
        source: source.into(),
        provenance,
        closed_thru,
        observed_at,
        low_n: None,
        unavailable: Some(reason),
        factor_errors: vec![],
        span: None,
        train_residuals: vec![],
        train_breaks: vec![],
        train_residual_mean: 0.,
        train_span_digests: vec![],
    }
}

/// The whole Lab computation. Admission is checked again here, before anything is keyed or fitted.
/// A requested factor with an error or a failed admission makes the report unavailable: there is
/// never a partial fit, a substituted peer or a fallback source.
#[allow(clippy::too_many_arguments)]
pub fn compute(
    target: &NamedBars,
    factors: &[NamedBars],
    spec: &LabSpec,
    tf: Timeframe,
    daily: bool,
    source: &str,
    closed_thru: i64,
    observed_at: Option<i64>,
    key: u64,
) -> LabReport {
    let provenance = std::iter::once(target).chain(factors).map(|r| Bar::provenance(&r.bars)).reduce(Provenance::combine).unwrap_or_default();
    let fail = |reason: String| blank(&target.symbol, tf, spec, source, closed_thru, observed_at, key, provenance, reason);
    if let Err(e) = spec.validate_for(&target.symbol) {
        return fail(e);
    }
    let mut factor_errors: Vec<(String, String)> = Vec::new();
    for row in std::iter::once(target).chain(factors) {
        let err = row.error.clone().or_else(|| crate::stats::require_research_bars(&row.bars, "Lab series").err());
        if let Some(e) = err {
            factor_errors.push((row.symbol.clone(), e));
        }
    }
    let wanted: Vec<&str> = spec.factors.iter().map(|f| f.symbol.as_str()).collect();
    if factors.len() != wanted.len() || factors.iter().zip(&wanted).any(|(r, w)| r.symbol != *w) {
        return fail("loaded factor rows do not match the requested factors".into());
    }
    if !factor_errors.is_empty() {
        let mut r = fail(format!(
            "Regression Lab needs every requested series; unavailable: {}. No partial fit and no substitute is used.",
            factor_errors
                .iter()
                .map(|(s, e)| format!("{} ({e})", crate::security(s).ticker))
                .collect::<Vec<_>>()
                .join("; ")
        ));
        r.factor_errors = factor_errors;
        return r;
    }
    let design = match build_design(target, factors, spec, tf, daily, source, closed_thru, observed_at) {
        Ok(d) => d,
        Err(e) => return fail(e),
    };
    let mut report = blank(&target.symbol, tf, spec, source, closed_thru, observed_at, key, provenance, String::new());
    report.unavailable = None;
    report.design_rows = design.y.len();
    report.target_bars = design.target_bars;
    report.dropped = design.dropped.clone();
    report.receipts = design.receipts.clone();
    report.grids = design.grids.clone();
    report.units = design.units.clone();
    report.span = design.ts.first().zip(design.ts.last()).map(|(a, b)| (*a, *b));
    let h_y = spec.target.horizon();
    let (train, purged, test, split_ts) = match split(&design, spec.holdout_pct, h_y) {
        Ok(s) => s,
        Err(e) => {
            report.unavailable = Some(e);
            return report;
        }
    };
    let k = spec.factors.len() + 1;
    let n_train = train.len();
    if n_train <= k {
        report.unavailable = Some(format!("need more rows than parameters: {n_train} training rows for k={k}"));
        return report;
    }
    let cols: Vec<&[f64]> = design.x.iter().map(|c| &c[..n_train]).collect();
    let y_train = &design.y[..n_train];
    let fit = match linalg::fit_ols(&cols, y_train) {
        Ok(f) => f,
        Err(e @ (LinalgError::Constant { .. } | LinalgError::RankDeficient { .. })) => {
            let name = |c: usize| {
                c.checked_sub(1).and_then(|j| spec.factors.get(j)).map_or("intercept".to_string(), LabSpec::term_label)
            };
            report.unavailable = Some(match e {
                LinalgError::Constant { column } => {
                    format!("{} is constant in the training span; the design cannot be fitted", name(column))
                }
                LinalgError::RankDeficient { rank, k, column } => {
                    format!("rank-deficient design (rank {rank} of {k}): {} is linearly dependent on the other terms", name(column))
                }
                _ => unreachable!(),
            });
            return report;
        }
        Err(e) => {
            report.unavailable = Some(e.to_string());
            return report;
        }
    };
    let h_all = spec.horizon_max();
    let low_n_train = n_train < min_train(k);
    let low_n_test = test.len() < MIN_TEST;
    if low_n_train || low_n_test {
        report.low_n = Some(format!(
            "LOW N: training rows {n_train} {} {} required (max(60, 10·k), k={k}); test rows {} {} {MIN_TEST}",
            if low_n_train { "<" } else { "≥" },
            min_train(k),
            test.len(),
            if low_n_test { "<" } else { "≥" },
        ));
    }
    // Covariance on Z (centred, training-scaled), then unscaled to original units.
    let z_rows = linalg::standardized_rows(&fit, &cols);
    let breaks: Vec<bool> = (0..n_train).map(|i| design.break_before[i]).collect();
    let beta = &fit.coefficients;
    if !low_n_train {
        for method in SE_METHODS {
            let mut cov = regression_covariance(&z_rows, &fit.residuals, &breaks, effective_method(method, h_all));
            if let Some(vz) = &cov.covariance {
                cov.covariance = Some(linalg::unscale_covariance(vz, &fit.mean, &fit.scale));
            }
            report.inference.push(inference_from(cov, beta));
        }
        // One BY family over the slope p-values per estimator; unavailable members enter at p = 1.
        for inf in &mut report.inference {
            let fam = Family::by("LAB slopes β_j=0 (all requested factors)", &inf.p[1..]);
            inf.q = std::iter::once(None).chain(fam.q.into_iter().map(Some)).collect();
        }
    }
    let y_bar = y_train.iter().sum::<f64>() / n_train as f64;
    let tss: f64 = y_train.iter().map(|v| (v - y_bar) * (v - y_bar)).sum();
    let r2_in = (tss > 0.).then(|| 1. - fit.sse / tss).and_then(finite);
    let r2_adj = r2_in.map(|r| 1. - (1. - r) * (n_train as f64 - 1.) / (n_train - k) as f64);
    let mut hs = Fnv64::default();
    hs.write_u64(key);
    hs.write_i64(split_ts);
    hs.write_i64(design.ts[n_train - 1]);
    hs.write_str(METHOD_SCHEMA);
    report.fit = Some(FittedModel {
        coefficients: fit.coefficients.clone(),
        mean: fit.mean.clone(),
        scale: fit.scale.clone(),
        residual_sigma: fit.sigma,
        train_mean: y_bar,
        n_train,
        k,
        rank: fit.rank,
        kappa_inf: fit.kappa_inf,
        vif: fit.vif.clone(),
        r2_in,
        r2_adj,
        train_start: design.ts[0],
        train_end: design.ts[n_train - 1],
        split_ts,
        spec: spec.clone(),
        input_identity: key,
        model_id: hs.finish(),
    });
    // Bounded training residuals and span digests, for freezing this fit (RESID). No refit later.
    let cut = n_train.saturating_sub(crate::studio_residual::MAX_TRAIN_RESIDUALS);
    report.train_residuals = fit.residuals[cut..].to_vec();
    report.train_breaks = (cut..n_train).map(|i| i == cut || design.break_before[i]).collect();
    report.train_residual_mean = fit.residuals.iter().sum::<f64>() / n_train as f64;
    report.train_span_digests = std::iter::once(target)
        .chain(factors)
        .map(|r| crate::studio_residual::span_digest(&r.bars, &r.symbol, design.ts[0], design.ts[n_train - 1]))
        .collect();
    // Frozen scoring of every row, in target space.
    let predict = |i: usize| beta[0] + (0..beta.len() - 1).map(|j| beta[j + 1] * design.x[j][i]).sum::<f64>();
    let yhat_test: Vec<f64> = test.iter().map(|&i| predict(i)).collect();
    let y_test: Vec<f64> = test.iter().map(|&i| design.y[i]).collect();
    let sse: f64 = y_test.iter().zip(&yhat_test).map(|(a, b)| (a - b) * (a - b)).sum();
    let baseline: f64 = y_test.iter().map(|a| (a - y_bar) * (a - y_bar)).sum();
    let mut holdout = LabHoldout {
        n_test: test.len(),
        purged,
        sse,
        baseline_sse: baseline,
        r2_oos: None,
        forecast_style: spec.factors.iter().all(|f| f.lag >= h_y),
        unavailable: None,
    };
    if low_n_test {
        holdout.unavailable = Some(format!("LOW N: {} test rows < {MIN_TEST} required; out-of-sample R² withheld", test.len()));
    } else {
        holdout.r2_oos = oos_r2(&y_test, &yhat_test, y_bar);
        if holdout.r2_oos.is_none() {
            holdout.unavailable = Some("test targets have no variance around the training mean".into());
        }
    }
    report.holdout = Some(holdout);
    report.diagnostics = Some(diagnostics(&fit.fitted, &fit.residuals, fit.sigma));
    let total = design.y.len();
    let from = total.saturating_sub(MAX_POINTS);
    report.points = (from..total)
        .map(|i| {
            let fitted = predict(i);
            LabPoint {
                ts: design.ts[i],
                y: design.y[i],
                fitted,
                residual: design.y[i] - fitted,
                break_before: design.break_before[i] || i == from,
                segment: if i >= total - test.len() {
                    Segment::Test
                } else if i >= n_train {
                    Segment::Purged
                } else {
                    Segment::Train
                },
            }
        })
        .collect();
    report
}

/// Statistical identity of a report: every row's symbol, source, gaps, error, provenance and bar
/// digest in spec order (target first), the spec key (which excludes the displayed covariance), the
/// clocks and the method schema. Pane identity is not part of it.
#[allow(clippy::too_many_arguments)]
pub fn cache_key(
    target: &NamedBars,
    factors: &[NamedBars],
    spec: &LabSpec,
    tf: Timeframe,
    source: &str,
    daily: bool,
    observed_at: Option<i64>,
    closed_thru: i64,
) -> crate::report_cache::ReportKey {
    let mut h = Fnv64::default();
    for row in std::iter::once(target).chain(factors) {
        h.write_str(&row.symbol);
        h.write_str(&row.source);
        h.write_str(&format!("{:?} {:?} {:?}", row.gaps, row.error, Bar::provenance(&row.bars)));
        h.write_u64(bars_digest(&row.bars));
    }
    h.write_str(&spec.key_string());
    h.write_str(&format!("daily={daily} observed={observed_at:?} closed_thru={closed_thru} {METHOD_SCHEMA}"));
    let mut key =
        crate::report_cache::ReportKey::new("studio_lab_v1", &target.symbol, tf, source, &target.bars, MODEL_NOTE);
    key.params = h.finish();
    key
}

fn inference_from(cov: RegressionCovariance, beta: &[f64]) -> CoefInference {
    let k = beta.len();
    let mut out = CoefInference {
        se: vec![None; k],
        interval: vec![None; k],
        z: vec![None; k],
        p: vec![None; k],
        q: vec![None; k],
        covariance: cov,
    };
    if let Some(v) = &out.covariance.covariance {
        for j in 0..k {
            let se = v[j][j];
            if se.is_finite() && se > 0. {
                let se = se.sqrt();
                let z = beta[j] / se;
                out.se[j] = Some(se);
                out.interval[j] = Some((beta[j] - 1.96 * se, beta[j] + 1.96 * se));
                out.z[j] = finite(z);
                out.p[j] = finite(z).map(normal_p);
            }
        }
    }
    out
}

/// Training-residual diagnostics, in-sample by construction.
pub fn diagnostics(fitted: &[f64], residuals: &[f64], sigma: f64) -> LabDiagnostics {
    let mut hist = vec![0u32; HIST_BINS];
    let mut beyond = 0;
    for e in residuals {
        let z = e / sigma;
        let b = ((z + 4.1) / 0.2).floor();
        if z.is_finite() && (0. ..HIST_BINS as f64).contains(&b) {
            hist[b as usize] += 1;
        } else {
            beyond += 1;
        }
    }
    let (cols, rows) = GRID_DIMS;
    let (xmin, xmax) = fitted.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(*v), b.max(*v)));
    let xspan = if xmax > xmin { xmax - xmin } else { 1. };
    let max_abs = residuals.iter().map(|e| e.abs()).fold(0., f64::max);
    let mut ylim = max_abs.min(6. * sigma);
    if !(ylim > 0.) {
        ylim = if sigma > 0. { sigma } else { 1. };
    }
    let mut grid = vec![0u32; cols * rows];
    let mut outside = 0;
    for (f, e) in fitted.iter().zip(residuals) {
        if e.abs() > ylim {
            outside += 1;
            continue;
        }
        let c = (((f - xmin) / xspan * cols as f64).floor() as usize).min(cols - 1);
        let r = (((e + ylim) / (2. * ylim) * rows as f64).floor() as usize).min(rows - 1);
        grid[r * cols + c] += 1;
    }
    LabDiagnostics {
        hist_edges: (-4.1, 4.1),
        hist,
        hist_beyond: beyond,
        grid,
        grid_dims: GRID_DIMS,
        grid_x: (xmin, xmin + xspan),
        grid_y: (-ylim, ylim),
        grid_outside: outside,
    }
}

#[cfg(test)]
#[path = "studio_lab_tests.rs"]
mod tests;
