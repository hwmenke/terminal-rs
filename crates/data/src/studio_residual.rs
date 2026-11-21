//! Frozen residual tickers (Studio slice 6). A verified Lab or Relate fit is frozen into an immutable
//! [`FrozenFit`]; the saved coefficients are then *scored* on live, re-admitted data, giving a signed
//! residual and a z series (a [`ScalarSeries`], never a `Bar`). Nothing here fits: `score` and `checks`
//! never call `fit_ols`, `regress` or `training_fit`, and a refit is a new Lab fit saved as a new version.
//!
//! Segments (per the saved `trained_thru`): **TRAIN** (`ts ≤ trained_thru`), **EMBARGO** (the next
//! `h_y − 1` target bars, whose h-bar support overlaps training) and **SCORED**. Checks use SCORED only.
//! Identity is a content digest over the statistical content, never a Lab `model_id` (which carries a
//! wall-clock observation time).
use crate::report_cache::ReportKey;
use crate::stable_hash::{Fnv64, bars_digest};
use crate::studio::{ScalarSeries, SeriesReceipt, SeriesUnit, Transform};
use crate::studio_corr::NamedBars;
use crate::studio_corr::relate::{MAX_POINTS, unit_for};
use crate::studio_corr::CorrReport;
use crate::studio_lab::{self, FactorSpec, LabReport, LabSpec};
pub use crate::studio_stationarity::{AdfResult, HalfLife};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tr_core::{Bar, Provenance, Timeframe};

pub const RESID_SCHEMA: u32 = 1;
/// Also written into every cache key.
pub const METHOD: &str = "resid.v1";
pub const MAX_TRAIN_RESIDUALS: usize = 5_000;
pub const MIN_SAVE_TRAIN: usize = 60;
pub const MIN_MONITOR: usize = 20;
pub const MAX_MONITOR: usize = 1_000;
pub const DEFAULT_MONITOR: usize = 250;
pub const DRAWS: usize = 499;
pub const MIN_ADF: usize = 60;
/// The loader limit is ≤ 6,000 bars, so this never binds in practice.
pub const MAX_SCORED: usize = 10_000;
/// Longest accepted name slug.
pub const MAX_SLUG: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FitOrigin {
    #[default]
    Lab,
    Relate,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SpanDigest {
    pub symbol: String,
    pub first_ts: i64,
    pub thru_ts: i64,
    pub n_bars: usize,
    pub digest: u64,
}

/// Digest of `(ts, close)` of the bars inside `[first_ts, thru_ts]`, plus their count.
pub fn span_digest(bars: &[Bar], symbol: &str, first_ts: i64, thru_ts: i64) -> SpanDigest {
    let mut h = Fnv64::default();
    let mut n = 0usize;
    for b in bars.iter().filter(|b| b.ts >= first_ts && b.ts <= thru_ts) {
        h.write_i64(b.ts);
        h.write_f64_bits(b.close);
        n += 1;
    }
    h.write_u64(n as u64);
    SpanDigest { symbol: symbol.into(), first_ts, thru_ts, n_bars: n, digest: h.finish() }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FrozenModel {
    /// Target security key.
    pub symbol: String,
    pub tf: Timeframe,
    pub target: Transform,
    /// Order == `coefficients[1..]`.
    pub factors: Vec<FactorSpec>,
    /// Original units, intercept first.
    pub coefficients: Vec<f64>,
    pub n_train: usize,
    /// Factors + 1.
    pub k: usize,
    pub train_start: i64,
    /// Lab only.
    pub kappa_inf: Option<f64>,
    /// Descriptive.
    pub r2_in: Option<f64>,
    pub origin: FitOrigin,
    /// Key of the report this was frozen from; hashed into the digest (it holds no wall clock).
    pub source_report_key: u64,
}
impl Default for FrozenModel {
    fn default() -> Self {
        Self {
            symbol: String::new(),
            tf: Timeframe::D1,
            target: Transform::LogReturn { h: 1 },
            factors: Vec::new(),
            coefficients: Vec::new(),
            n_train: 0,
            k: 0,
            train_start: 0,
            kappa_inf: None,
            r2_in: None,
            origin: FitOrigin::Lab,
            source_report_key: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FrozenFit {
    /// `{slug}-v{n}`; not in the digest.
    pub id: String,
    pub name: String,
    /// Previous version of the same slug; not in the digest.
    pub parent: Option<String>,
    /// Display only; not in the digest or any cache key.
    pub saved_at: i64,
    pub schema: u32,
    pub method: String,
    pub model: FrozenModel,
    /// μ over all `n_train` training residuals.
    pub residual_mean: f64,
    /// √(SSE/(n−k)): the df-corrected value, not the population SD.
    pub residual_sigma: f64,
    /// Last training target timestamp.
    pub trained_thru: i64,
    /// `h_y − 1`.
    pub embargo_bars: usize,
    /// Mandatory; `len == k`, target first.
    pub training_receipts: Vec<SeriesReceipt>,
    pub span_digests: Vec<SpanDigest>,
    /// Latest ≤ `MAX_TRAIN_RESIDUALS`, chronological.
    pub train_residuals: Vec<f64>,
    pub train_breaks: Vec<bool>,
    pub train_residuals_dropped: usize,
    pub monitoring_horizon: usize,
    /// `max(⌈n_stored^{1/3}⌉, h_y)`.
    pub bootstrap_block: usize,
    /// Coefficient covariances in original units, intercept first (Lab: `SE_METHODS` order, 4 entries;
    /// Relate: the fit's one). Empty on models saved before slice 7 → coefficient SE unavailable.
    #[serde(default)]
    pub coef_covariance: Vec<crate::studio_inference::RegressionCovariance>,
    pub content_digest: u64,
}
impl Default for FrozenFit {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            parent: None,
            saved_at: 0,
            schema: RESID_SCHEMA,
            method: METHOD.into(),
            model: FrozenModel::default(),
            residual_mean: 0.,
            residual_sigma: 0.,
            trained_thru: 0,
            embargo_bars: 0,
            training_receipts: Vec::new(),
            span_digests: Vec::new(),
            train_residuals: Vec::new(),
            train_breaks: Vec::new(),
            train_residuals_dropped: 0,
            monitoring_horizon: DEFAULT_MONITOR,
            bootstrap_block: 1,
            coef_covariance: Vec::new(),
            content_digest: 0,
        }
    }
}

/// Stable integer tags for the digest. Explicit (never `Debug`), so adding a field to a shared type or
/// changing a `Debug` impl cannot invalidate saved models. Adding an enum variant is a compile error here.
fn tf_tag(tf: Timeframe) -> u64 {
    match tf {
        Timeframe::M1 => 1,
        Timeframe::M5 => 2,
        Timeframe::M15 => 3,
        Timeframe::M30 => 4,
        Timeframe::H1 => 5,
        Timeframe::H4 => 6,
        Timeframe::D1 => 7,
        Timeframe::W1 => 8,
        Timeframe::MN1 => 9,
    }
}
fn write_transform(h: &mut Fnv64, t: Transform) {
    let (tag, n) = match t {
        Transform::Level => (1, 0),
        Transform::Change { h } => (2, h),
        Transform::LogReturn { h } => (3, h),
        Transform::LogLevel => (4, 0),
    };
    h.write_u64(tag);
    h.write_u64(n as u64);
}
fn write_opt_f64(h: &mut Fnv64, v: Option<f64>) {
    match v {
        Some(x) => {
            h.write_u64(1);
            h.write_f64_bits(x);
        }
        None => h.write_u64(0),
    }
}
fn provenance_tag(p: Provenance) -> u64 {
    match p {
        Provenance::Real => 1,
        Provenance::Cached => 2,
        Provenance::Sample => 3,
        Provenance::Missing => 4,
    }
}
/// Receipt identity: source, provenance, `closed_thru`, `input_digest`, gaps. Not `observed_at`, which is
/// the (wall-clock) load time for daily data.
fn write_receipt(h: &mut Fnv64, r: &SeriesReceipt) {
    h.write_str(&r.source);
    h.write_u64(provenance_tag(r.provenance));
    h.write_i64(r.closed_thru);
    h.write_u64(r.input_digest);
    h.write_u64(r.gaps.len() as u64);
    for (a, b) in &r.gaps {
        h.write_i64(*a);
        h.write_i64(*b);
    }
}

/// Explicit method tags (never `Debug`).
fn write_covariance(h: &mut Fnv64, c: &crate::studio_inference::RegressionCovariance) {
    use crate::studio_inference::CovarianceMethod as M;
    match c.method {
        M::Classical => h.write_u64(1),
        M::HC3 => h.write_u64(2),
        M::HacFixed5 => h.write_u64(3),
        M::HacAutomatic => h.write_u64(4),
        M::Overlap(n) => {
            h.write_u64(5);
            h.write_u64(n as u64);
        }
    }
    h.write_u64(c.lag as u64);
    h.write_u64(c.n as u64);
    h.write_u64(c.fallback as u64);
    match &c.covariance {
        Some(v) => {
            h.write_u64(1);
            for row in v {
                for x in row {
                    h.write_f64_bits(*x);
                }
            }
        }
        None => h.write_u64(0),
    }
    match &c.unavailable {
        Some(u) => {
            h.write_u64(1);
            h.write_str(u);
        }
        None => h.write_u64(0),
    }
}

impl FrozenFit {
    /// FNV-64 over every field except `id`, `name`, `parent`, `saved_at` and `content_digest`,
    /// field by field with explicit tags (no `Debug` text), and without any receipt's `observed_at`.
    /// `source_report_key` is included: it is a deterministic hash of keyed content and embeds no wall
    /// clock (the observation clock is deliberately not part of the report key). `f64` via bit patterns,
    /// strings length-prefixed.
    pub fn compute_digest(&self) -> u64 {
        let mut h = Fnv64::default();
        h.write_str(METHOD);
        h.write_u64(u64::from(self.schema));
        h.write_str(&self.method);
        let m = &self.model;
        h.write_str(&m.symbol);
        h.write_u64(tf_tag(m.tf));
        write_transform(&mut h, m.target);
        h.write_u64(m.factors.len() as u64);
        for f in &m.factors {
            h.write_str(&f.symbol);
            write_transform(&mut h, f.transform);
            h.write_u64(f.lag as u64);
        }
        h.write_u64(m.coefficients.len() as u64);
        for c in &m.coefficients {
            h.write_f64_bits(*c);
        }
        h.write_u64(m.n_train as u64);
        h.write_u64(m.k as u64);
        h.write_i64(m.train_start);
        write_opt_f64(&mut h, m.kappa_inf);
        write_opt_f64(&mut h, m.r2_in);
        h.write_u64(match m.origin {
            FitOrigin::Lab => 1,
            FitOrigin::Relate => 2,
        });
        h.write_u64(m.source_report_key);
        h.write_f64_bits(self.residual_mean);
        h.write_f64_bits(self.residual_sigma);
        h.write_i64(self.trained_thru);
        h.write_u64(self.embargo_bars as u64);
        h.write_u64(self.training_receipts.len() as u64);
        for r in &self.training_receipts {
            write_receipt(&mut h, r);
        }
        for s in &self.span_digests {
            h.write_str(&s.symbol);
            h.write_i64(s.first_ts);
            h.write_i64(s.thru_ts);
            h.write_u64(s.n_bars as u64);
            h.write_u64(s.digest);
        }
        h.write_u64(self.train_residuals.len() as u64);
        for e in &self.train_residuals {
            h.write_f64_bits(*e);
        }
        for b in &self.train_breaks {
            h.write_u64(u64::from(*b));
        }
        h.write_u64(self.train_residuals_dropped as u64);
        h.write_u64(self.monitoring_horizon as u64);
        h.write_u64(self.bootstrap_block as u64);
        // Only when present, so every model saved before slice 7 keeps its digest.
        if !self.coef_covariance.is_empty() {
            h.write_str("coef_cov.v1");
            h.write_u64(self.coef_covariance.len() as u64);
            for c in &self.coef_covariance {
                write_covariance(&mut h, c);
            }
        }
        h.finish()
    }

    /// Structural and numeric validity, including the digest. Never trusts a loaded file.
    pub fn validate(&self) -> Result<(), String> {
        let m = &self.model;
        if self.schema != RESID_SCHEMA {
            return Err(format!("schema {} is not {RESID_SCHEMA}", self.schema));
        }
        if self.method != METHOD {
            return Err(format!("method `{}` is not `{METHOD}`", self.method));
        }
        if m.k != m.factors.len() + 1 || m.k != m.coefficients.len() {
            return Err("k does not match the factors and coefficients".into());
        }
        if m.coefficients.iter().any(|c| !c.is_finite())
            || !self.residual_mean.is_finite()
            || self.train_residuals.iter().any(|e| !e.is_finite())
        {
            return Err("non-finite saved value".into());
        }
        if !(self.residual_sigma.is_finite() && self.residual_sigma > 0.) {
            return Err("saved σ must be finite and positive".into());
        }
        if self.training_receipts.len() != m.k || self.span_digests.len() != m.k {
            return Err("training receipts / span digests do not cover every input".into());
        }
        if self.train_residuals.len() != self.train_breaks.len() || self.train_residuals.is_empty() {
            return Err("training residuals and breaks disagree".into());
        }
        if !(MIN_MONITOR..=MAX_MONITOR).contains(&self.monitoring_horizon)
            || self.monitoring_horizon > self.train_residuals.len()
        {
            return Err("monitoring horizon is out of range".into());
        }
        let stored = self.train_residuals.len();
        if self.bootstrap_block == 0 || self.bootstrap_block > stored {
            return Err("bootstrap block must be between 1 and the stored residual count".into());
        }
        if self.bootstrap_block < self.h_y() {
            return Err("bootstrap block is shorter than the return horizon".into());
        }
        if self.embargo_bars != self.h_y().saturating_sub(1) || self.h_y() == 0 {
            return Err("embargo does not equal the return horizon minus one".into());
        }
        if !self.train_breaks[0] {
            return Err("the first training residual must start a segment".into());
        }
        if stored > MAX_TRAIN_RESIDUALS || stored > m.n_train {
            return Err("more stored training residuals than allowed or than were trained on".into());
        }
        if self.train_residuals_dropped != m.n_train - stored {
            return Err("dropped training residual count is inconsistent".into());
        }
        self.lab_spec().validate()?;
        let want_len: &[usize] = match m.origin {
            FitOrigin::Lab => &[0, 4],
            FitOrigin::Relate => &[0, 1],
        };
        if !want_len.contains(&self.coef_covariance.len()) {
            return Err("coefficient covariance count does not match the model origin".into());
        }
        for c in &self.coef_covariance {
            let Some(v) = &c.covariance else { continue };
            if v.len() != m.k || v.iter().any(|r| r.len() != m.k) || v.iter().flatten().any(|x| !x.is_finite()) {
                return Err("coefficient covariance must be a finite k×k matrix".into());
            }
            let big = v.iter().flatten().fold(0f64, |a, x| a.max(x.abs()));
            for i in 0..m.k {
                if v[i][i] < 0. {
                    return Err("coefficient covariance has a negative variance".into());
                }
                for j in 0..i {
                    if (v[i][j] - v[j][i]).abs() > 1e-9 * big {
                        return Err("coefficient covariance is not symmetric".into());
                    }
                }
            }
        }
        if self.compute_digest() != self.content_digest {
            return Err("content digest does not match the saved content".into());
        }
        Ok(())
    }

    /// The one-model LabSpec this fit scores with.
    pub fn lab_spec(&self) -> LabSpec {
        LabSpec { target: self.model.target, factors: self.model.factors.clone(), ..Default::default() }
    }
    pub fn h_y(&self) -> usize {
        self.model.target.horizon()
    }
    /// `{slug}` of an id (`slug-v3` → `slug`).
    pub fn slug(&self) -> &str {
        self.id.rsplit_once("-v").map_or(self.id.as_str(), |(s, _)| s)
    }
}

/// The provider part of a source label. The intraday vault label (`StitchInfo::label`) is
/// `"{source} · VAULT n + LIVE m bars · … holes · … missing · {note}"`, whose counts and notes change
/// whenever a bar arrives; only the leading provider name is identity.
pub fn source_identity(label: &str) -> &str {
    label.split(" · VAULT ").next().unwrap_or(label).trim()
}

fn valid_slug(s: &str) -> bool {
    (1..=MAX_SLUG).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

/// `^[A-Za-z0-9_-]{1,40}-v[1-9][0-9]{0,4}$`. No `.` or `/`, so an id is always a safe file stem.
pub fn valid_id(id: &str) -> bool {
    let Some((slug, n)) = id.rsplit_once("-v") else { return false };
    valid_slug(slug)
        && (1..=5).contains(&n.len())
        && n.chars().all(|c| c.is_ascii_digit())
        && !n.starts_with('0')
}

pub fn valid_name(name: &str) -> bool {
    valid_slug(name)
}

/// Words after `STUDIO resid` / `RESID`.
#[derive(Debug, Clone, PartialEq)]
pub enum ResidArgs {
    List,
    Save { name: Option<String>, monitor: Option<usize> },
    Open(String),
    Check(Option<String>),
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"').and_then(|t| t.strip_suffix('"')).unwrap_or(s)
}

impl ResidArgs {
    pub fn parse(words: &str) -> Result<Self, String> {
        let w: Vec<&str> = words.split_whitespace().collect();
        let Some(first) = w.first() else { return Ok(Self::List) };
        let id = |s: &str| {
            let s = unquote(s);
            valid_id(s).then(|| s.to_string()).ok_or_else(|| format!("`{s}` is not a model id like AAPL-v-SPY-v1"))
        };
        match first.to_ascii_lowercase().as_str() {
            "open" => match w.as_slice() {
                [_, one] => Ok(Self::Open(id(one)?)),
                _ => Err("usage: STUDIO resid open <id>".into()),
            },
            "check" => match w.as_slice() {
                [_] => Ok(Self::Check(None)),
                [_, one] => Ok(Self::Check(Some(id(one)?))),
                _ => Err("usage: STUDIO resid check [<id>]".into()),
            },
            "save" => Self::parse_save(&w[1..]),
            f if f.starts_with("name=") => Self::parse_save(&w),
            _ => Err(format!("unknown RESID input `{first}`; use save, open or check")),
        }
    }
    fn parse_save(words: &[&str]) -> Result<Self, String> {
        let (mut name, mut monitor) = (None, None);
        for word in words {
            let Some((key, value)) = word.split_once('=') else {
                return Err(format!("unknown RESID save input `{word}`"));
            };
            match key.to_ascii_lowercase().as_str() {
                "name" if name.is_none() => {
                    let v = unquote(value);
                    if !valid_name(v) {
                        return Err("name must be 1–40 characters of A–Z a–z 0–9 _ -".into());
                    }
                    name = Some(v.to_string());
                }
                "monitor" if monitor.is_none() => {
                    monitor = Some(
                        value
                            .parse::<usize>()
                            .ok()
                            .filter(|m| (MIN_MONITOR..=MAX_MONITOR).contains(m))
                            .ok_or(format!("monitor must be {MIN_MONITOR}–{MAX_MONITOR}"))?,
                    );
                }
                _ => return Err(format!("unknown or repeated RESID save input `{word}`")),
            }
        }
        Ok(Self::Save { name, monitor })
    }
}

/// The report a freeze starts from (an immutable `Arc` shown by a pane).
#[derive(Debug, Clone)]
pub enum FitSource {
    Lab(Arc<LabReport>),
    Relate { report: Arc<CorrReport>, tf: Timeframe },
}

struct Parts {
    symbol: String,
    tf: Timeframe,
    target: Transform,
    factors: Vec<FactorSpec>,
    coefficients: Vec<f64>,
    n_train: usize,
    train_start: i64,
    trained_thru: i64,
    kappa_inf: Option<f64>,
    r2_in: Option<f64>,
    origin: FitOrigin,
    source_report_key: u64,
    sigma: f64,
    mean: f64,
    receipts: Vec<SeriesReceipt>,
    spans: Vec<SpanDigest>,
    residuals: Vec<f64>,
    breaks: Vec<bool>,
    coef_covariance: Vec<crate::studio_inference::RegressionCovariance>,
}

/// Default name `{TARGET}-v-{F1}` with `_p{n}` for `n` further factors, sanitised to the slug alphabet.
fn default_name(symbol: &str, factors: &[FactorSpec]) -> String {
    let t = crate::security(symbol).ticker;
    let f = factors.first().map_or(String::new(), |f| crate::security(&f.symbol).ticker);
    let mut s = format!("{t}-v-{f}");
    if factors.len() > 1 {
        s += &format!("_p{}", factors.len() - 1);
    }
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).take(MAX_SLUG).collect()
}

fn assemble(p: Parts, name: &str, monitor: Option<usize>, saved_at: i64) -> Result<FrozenFit, String> {
    let k = p.factors.len() + 1;
    if p.receipts.len() != k || p.spans.len() != k || p.residuals.is_empty() || p.residuals.len() != p.breaks.len() {
        return Err("this report predates residual saving; Reload the report and save again".into());
    }
    let need = MIN_SAVE_TRAIN.max(studio_lab::min_train(k));
    if p.n_train < need {
        return Err(format!("LOW N: {} training rows < {need} required to save a residual model", p.n_train));
    }
    let stored = p.residuals.len();
    let monitor = match monitor {
        Some(m) if !(MIN_MONITOR..=MAX_MONITOR).contains(&m) => {
            return Err(format!("monitor must be {MIN_MONITOR}–{MAX_MONITOR}"));
        }
        Some(m) if m > stored => return Err(format!("monitor {m} exceeds the {stored} stored training residuals")),
        Some(m) => m,
        None if stored < MIN_MONITOR => {
            return Err(format!("only {stored} training residuals are stored; need {MIN_MONITOR} to monitor"));
        }
        None => DEFAULT_MONITOR.min(stored),
    };
    if !valid_name(name) {
        return Err("name must be 1–40 characters of A–Z a–z 0–9 _ -".into());
    }
    let h_y = p.target.horizon();
    let cube = (stored as f64).cbrt().ceil() as usize;
    let mut fit = FrozenFit {
        id: String::new(),
        name: name.into(),
        parent: None,
        saved_at,
        schema: RESID_SCHEMA,
        method: METHOD.into(),
        model: FrozenModel {
            symbol: p.symbol,
            tf: p.tf,
            target: p.target,
            factors: p.factors,
            coefficients: p.coefficients,
            n_train: p.n_train,
            k,
            train_start: p.train_start,
            kappa_inf: p.kappa_inf,
            r2_in: p.r2_in,
            origin: p.origin,
            source_report_key: p.source_report_key,
        },
        residual_mean: p.mean,
        residual_sigma: p.sigma,
        trained_thru: p.trained_thru,
        embargo_bars: h_y.saturating_sub(1),
        training_receipts: p.receipts,
        span_digests: p.spans,
        train_residuals: p.residuals,
        train_breaks: p.breaks,
        train_residuals_dropped: p.n_train.saturating_sub(stored),
        monitoring_horizon: monitor,
        bootstrap_block: cube.max(h_y).max(1),
        coef_covariance: p.coef_covariance,
        content_digest: 0,
    };
    fit.content_digest = fit.compute_digest();
    fit.validate().map_err(|e| format!("cannot save: {e}"))?;
    Ok(fit)
}

/// Freeze a displayed Lab fit. Makes no new fit and no new load.
pub fn freeze_lab(r: &LabReport, name: Option<&str>, monitor: Option<usize>, saved_at: i64) -> Result<FrozenFit, String> {
    r.provenance.require_verified("Residual save")?;
    if let Some(u) = &r.unavailable {
        return Err(format!("nothing to save: {u}"));
    }
    let Some(f) = &r.fit else { return Err("nothing to save: the Lab has no fit".into()) };
    if let Some(low) = &r.low_n {
        // A LOW N fit keeps descriptive values but is never frozen into a monitored model.
        if f.n_train < MIN_SAVE_TRAIN.max(studio_lab::min_train(f.k)) {
            return Err(low.clone());
        }
    }
    let name = name.map_or_else(|| default_name(&r.symbol, &r.spec.factors), str::to_string);
    assemble(
        Parts {
            symbol: r.symbol.clone(),
            tf: r.tf,
            target: r.spec.target,
            factors: r.spec.factors.clone(),
            coefficients: f.coefficients.clone(),
            n_train: f.n_train,
            train_start: f.train_start,
            trained_thru: f.train_end,
            kappa_inf: Some(f.kappa_inf),
            r2_in: f.r2_in,
            origin: FitOrigin::Lab,
            source_report_key: r.key,
            sigma: f.residual_sigma,
            mean: r.train_residual_mean,
            receipts: r.receipts.clone(),
            spans: r.train_span_digests.clone(),
            residuals: r.train_residuals.clone(),
            breaks: r.train_breaks.clone(),
            coef_covariance: r.inference.iter().map(|i| i.covariance.clone()).collect(),
        },
        &name,
        monitor,
        saved_at,
    )
}

/// Freeze the single-factor Relate fit as a one-factor model `target : benchmark : lag 0`.
pub fn freeze_relate(
    r: &CorrReport,
    tf: Timeframe,
    name: Option<&str>,
    monitor: Option<usize>,
    saved_at: i64,
) -> Result<FrozenFit, String> {
    r.provenance.require_verified("Residual save")?;
    let Some(rel) = &r.relate else { return Err("nothing to save: the Relate section is absent".into()) };
    if let Some(u) = &rel.unavailable {
        return Err(format!("nothing to save: {u}"));
    }
    let Some(f) = &rel.fit else { return Err("nothing to save: Relate has no fit".into()) };
    // The timeframe comes from the report; `tf` is only the pane's current request and must agree.
    let Some(report_tf) = rel.tf else {
        return Err("this Relate report predates saved timeframes: Reload it, then save".into());
    };
    if report_tf != tf {
        return Err(format!(
            "save refused: the report was computed at {} but the pane now says {}; press Reload or restore the timeframe",
            report_tf.label(),
            tf.label()
        ));
    }
    let tf = report_tf;
    let factors = vec![FactorSpec { symbol: r.benchmark.clone(), transform: f.transform, lag: 0 }];
    let name = name.map_or_else(|| default_name(&r.symbol, &factors), str::to_string);
    assemble(
        Parts {
            symbol: r.symbol.clone(),
            tf,
            target: f.transform,
            factors,
            coefficients: vec![f.alpha, f.beta],
            n_train: f.n,
            train_start: rel.train_start,
            trained_thru: f.train_end,
            kappa_inf: None,
            r2_in: None,
            origin: FitOrigin::Relate,
            source_report_key: rel.key,
            sigma: f.sigma,
            mean: rel.train_residual_mean,
            receipts: vec![rel.y.receipt.clone(), rel.x.receipt.clone()],
            spans: rel.train_span_digests.clone(),
            residuals: rel.train_residuals.clone(),
            breaks: rel.train_breaks.clone(),
            coef_covariance: vec![f.covariance.clone()],
        },
        &name,
        monitor,
        saved_at,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResidSegment {
    Train,
    Embargo,
    Scored,
}
impl ResidSegment {
    pub fn label(self) -> &'static str {
        match self {
            Self::Train => "TRAIN · in-sample",
            Self::Embargo => "EMBARGO",
            Self::Scored => "SCORED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputCheck {
    Unchanged,
    Revised { live: u64, saved: u64 },
    NotCovered { live_first_ts: i64 },
    SourceChanged { saved: String, live: String },
}

/// Plan type, built on demand from a report and never stored.
#[derive(Debug, Clone, PartialEq)]
pub struct ResidualPoint {
    pub ts: i64,
    pub residual: Option<f64>,
    pub z: Option<f64>,
    pub break_before: bool,
    pub segment: ResidSegment,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResidualReport {
    pub key: u64,
    pub model_id: String,
    pub model_digest: u64,
    /// Target-transform units, latest ≤ `MAX_POINTS` target bars, `None` = missing.
    pub residual: ScalarSeries,
    /// `SeriesUnit::ZFrozen`, same ts / breaks.
    pub z: ScalarSeries,
    pub segment: Vec<ResidSegment>,
    /// Frozen design on the target grid (hover and ATTR), parallel to `residual.ts`.
    pub y: Vec<Option<f64>>,
    pub fitted: Vec<Option<f64>>,
    /// Regressors `x_{j,t−ℓ_j}` per factor, parallel to `residual.ts` (hover and ATTR).
    #[serde(default)]
    pub x: Vec<Vec<Option<f64>>>,
    pub scored: Vec<f64>,
    pub scored_ts: Vec<i64>,
    pub scored_breaks: Vec<bool>,
    pub scored_missing: usize,
    pub revision: Vec<(String, InputCheck)>,
    pub dropped: Vec<(String, usize)>,
    pub receipts: Vec<SeriesReceipt>,
    pub source: String,
    pub provenance: Provenance,
    pub closed_thru: i64,
    pub observed_at: Option<i64>,
    pub factor_errors: Vec<(String, String)>,
    pub unavailable: Option<String>,
}

impl ResidualReport {
    pub fn point(&self, i: usize) -> ResidualPoint {
        ResidualPoint {
            ts: self.residual.ts[i],
            residual: self.residual.values[i],
            z: self.z.values[i],
            break_before: self.residual.break_before[i],
            segment: self.segment[i],
        }
    }
    /// Latest-first nearest index to a timestamp (binary search on the ascending grid).
    pub fn nearest(&self, ts: i64) -> Option<usize> {
        let t = &self.residual.ts;
        if t.is_empty() {
            return None;
        }
        let i = t.partition_point(|x| *x < ts);
        Some(match i {
            0 => 0,
            i if i == t.len() => i - 1,
            i => if ts - t[i - 1] <= t[i] - ts { i - 1 } else { i },
        })
    }
}

fn empty_series(unit: SeriesUnit, receipt: SeriesReceipt) -> ScalarSeries {
    ScalarSeries { ts: vec![], values: vec![], break_before: vec![], unit, receipt }
}

fn blank_report(
    fit: &FrozenFit,
    source: &str,
    provenance: Provenance,
    closed_thru: i64,
    observed_at: Option<i64>,
    key: u64,
    reason: String,
) -> ResidualReport {
    let receipt = SeriesReceipt {
        source: source.into(),
        provenance,
        observed_at,
        closed_thru,
        input_digest: 0,
        gaps: vec![],
    };
    ResidualReport {
        key,
        model_id: fit.id.clone(),
        model_digest: fit.content_digest,
        residual: empty_series(unit_for(fit.model.target), receipt.clone()),
        z: empty_series(SeriesUnit::ZFrozen, receipt),
        segment: vec![],
        y: vec![],
        fitted: vec![],
        x: vec![],
        scored: vec![],
        scored_ts: vec![],
        scored_breaks: vec![],
        scored_missing: 0,
        revision: vec![],
        dropped: vec![],
        receipts: vec![],
        source: source.into(),
        provenance,
        closed_thru,
        observed_at,
        factor_errors: vec![],
        unavailable: Some(reason),
    }
}

/// Identity of a scored report: the model's content digest, every live row's symbol, source, gaps,
/// error, provenance and bar digest in model order (target first), and the closed boundary. The
/// observation clock is **not** keyed (a daily `observed_at` is a load time); a correction inside an
/// old bar changes `bars_digest` and so the key.
pub fn score_key(
    fit: &FrozenFit,
    target: &NamedBars,
    factors: &[NamedBars],
    source: &str,
    daily: bool,
    closed_thru: i64,
) -> ReportKey {
    let mut h = Fnv64::default();
    h.write_u64(fit.content_digest);
    for row in std::iter::once(target).chain(factors) {
        h.write_str(&row.symbol);
        h.write_str(&row.source);
        h.write_str(&format!("{:?} {:?} {:?}", row.gaps, row.error, Bar::provenance(&row.bars)));
        h.write_u64(bars_digest(&row.bars));
    }
    h.write_str(source);
    h.write_str(&format!("daily={daily} closed_thru={closed_thru} {METHOD} max_points={MAX_POINTS}"));
    let mut key = ReportKey::new("studio_resid_v1", &target.symbol, fit.model.tf, source, &target.bars, METHOD);
    key.params = h.finish();
    key
}

/// Scores `fit` on live rows. **Never fits**: coefficients, μ and σ come from `fit` only.
/// `e_t = y_t − (β₀ + Σ β_j x_{j,t−ℓ_j})`, `z_t = (e_t − μ_saved)/σ_saved`, on the strict grid of
/// `build_design`. A missing factor makes the report unavailable; a missing timestamp gives `None`.
#[allow(clippy::too_many_arguments)]
pub fn score(
    fit: &FrozenFit,
    target: &NamedBars,
    factors: &[NamedBars],
    daily: bool,
    source: &str,
    closed_thru: i64,
    observed_at: Option<i64>,
    key: u64,
) -> ResidualReport {
    let provenance =
        std::iter::once(target).chain(factors).map(|r| Bar::provenance(&r.bars)).reduce(Provenance::combine).unwrap_or_default();
    let fail = |reason: String| blank_report(fit, source, provenance, closed_thru, observed_at, key, reason);
    if let Err(e) = fit.validate() {
        return fail(format!("model invalid: {e}"));
    }
    if target.symbol != fit.model.symbol {
        return fail(format!("loaded target {} does not match the model's {}", target.symbol, fit.model.symbol));
    }
    let mut factor_errors: Vec<(String, String)> = Vec::new();
    for row in std::iter::once(target).chain(factors) {
        let err = row.error.clone().or_else(|| crate::stats::require_research_bars(&row.bars, "Residual input").err());
        if let Some(e) = err {
            factor_errors.push((row.symbol.clone(), e));
        }
    }
    if factors.len() != fit.model.factors.len() || factors.iter().zip(&fit.model.factors).any(|(r, f)| r.symbol != f.symbol) {
        return fail("loaded factor rows do not match the model's factors".into());
    }
    if !factor_errors.is_empty() {
        let mut r = fail(format!(
            "Residual scoring needs every saved input; unavailable: {}. The saved model is never re-estimated on fewer factors.",
            factor_errors
                .iter()
                .map(|(s, e)| format!("{} ({e})", crate::security(s).ticker))
                .collect::<Vec<_>>()
                .join("; ")
        ));
        r.factor_errors = factor_errors;
        return r;
    }
    let design = match studio_lab::build_design(target, factors, &fit.lab_spec(), fit.model.tf, daily, source, closed_thru, observed_at)
    {
        Ok(d) => d,
        Err(e) => return fail(e),
    };
    let beta = &fit.model.coefficients;
    let (mu, sigma) = (fit.residual_mean, fit.residual_sigma);
    let rows = design.y.len();
    let mut fitted = Vec::with_capacity(rows);
    let mut resid = Vec::with_capacity(rows);
    for i in 0..rows {
        let yhat = beta[0] + (0..beta.len() - 1).map(|j| beta[j + 1] * design.x[j][i]).sum::<f64>();
        fitted.push(yhat);
        resid.push(design.y[i] - yhat);
    }
    let n_bars = design.target_bars;
    let a = target.bars.partition_point(|b| b.ts <= fit.trained_thru);
    let scored_from = a.saturating_add(fit.embargo_bars);
    let from = n_bars.saturating_sub(MAX_POINTS);
    let mut ts = Vec::with_capacity(n_bars - from);
    let (mut values, mut zs, mut y, mut yh) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut xs: Vec<Vec<Option<f64>>> = vec![Vec::with_capacity(n_bars - from); design.x.len()];
    let mut brk: Vec<bool> = Vec::new();
    let mut segment = Vec::new();
    let mut ri = design.bar.partition_point(|b| *b < from);
    for t in from..n_bars {
        ts.push(target.bars[t].ts);
        segment.push(if target.bars[t].ts <= fit.trained_thru {
            ResidSegment::Train
        } else if t < scored_from {
            ResidSegment::Embargo
        } else {
            ResidSegment::Scored
        });
        if ri < rows && design.bar[ri] == t {
            let (e, yhat) = (resid[ri], fitted[ri]);
            let prev_some = values.last().is_some_and(|v: &Option<f64>| v.is_some());
            brk.push(t == from || !prev_some || design.break_before[ri]);
            values.push(Some(e));
            zs.push(Some((e - mu) / sigma));
            y.push(Some(design.y[ri]));
            yh.push(Some(yhat));
            for (col, d) in xs.iter_mut().zip(&design.x) {
                col.push(Some(d[ri]));
            }
            ri += 1;
        } else {
            brk.push(false);
            values.push(None);
            zs.push(None);
            y.push(None);
            yh.push(None);
            for col in &mut xs {
                col.push(None);
            }
        }
    }
    let target_receipt = design.receipts[0].clone();
    let mut scored = Vec::new();
    let mut scored_ts = Vec::new();
    let mut scored_breaks = Vec::new();
    for i in 0..rows {
        if design.bar[i] >= scored_from && scored.len() < MAX_SCORED {
            scored_breaks.push(scored.is_empty() || design.break_before[i]);
            scored_ts.push(design.ts[i]);
            scored.push(resid[i]);
        }
    }
    let scored_bars = n_bars.saturating_sub(scored_from);
    let rows_in_scored = design.bar.iter().filter(|b| **b >= scored_from).count();
    // Revision of every live input against the saved training-span digests.
    let day = |t: i64| if daily { t.div_euclid(86_400) } else { t };
    let revision: Vec<(String, InputCheck)> = std::iter::once(target)
        .chain(factors)
        .zip(&fit.span_digests)
        .zip(&fit.training_receipts)
        .map(|((row, saved), receipt)| {
            let live = span_digest(&row.bars, &row.symbol, saved.first_ts, saved.thru_ts);
            let first = row.bars.first().map(|b| b.ts);
            let check = if first.is_none_or(|f| day(f) > day(saved.first_ts)) {
                InputCheck::NotCovered { live_first_ts: first.unwrap_or(0) }
            } else if live.digest != saved.digest || live.n_bars != saved.n_bars {
                InputCheck::Revised { live: live.digest, saved: saved.digest }
            } else if source_identity(&row.source) != source_identity(&receipt.source) {
                InputCheck::SourceChanged { saved: receipt.source.clone(), live: row.source.clone() }
            } else {
                InputCheck::Unchanged
            };
            (row.symbol.clone(), check)
        })
        .collect();
    let make = |values: Vec<Option<f64>>, unit: SeriesUnit| ScalarSeries {
        ts: ts.clone(),
        values,
        break_before: brk.clone(),
        unit,
        receipt: target_receipt.clone(),
    };
    ResidualReport {
        key,
        model_id: fit.id.clone(),
        model_digest: fit.content_digest,
        residual: make(values, unit_for(fit.model.target)),
        z: make(zs, SeriesUnit::ZFrozen),
        segment,
        y,
        fitted: yh,
        x: xs,
        scored,
        scored_ts,
        scored_breaks,
        scored_missing: scored_bars.saturating_sub(rows_in_scored),
        revision,
        dropped: design.dropped.clone(),
        receipts: design.receipts.clone(),
        source: source.into(),
        provenance,
        closed_thru,
        observed_at,
        factor_errors: vec![],
        unavailable: None,
    }
}

// ───────────────────────────── checks ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum CheckState<T> {
    Pending { have: usize, need: usize, why: String },
    Ready(T),
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorResult {
    /// Fixed horizon.
    pub m: usize,
    /// Observed scored residuals used (≤ m).
    pub k: usize,
    /// `S_1 … S_k`.
    pub path: Vec<f64>,
    pub d_k: f64,
    /// Pointwise 2.5 / 97.5% of `S*_j`, length m.
    pub envelope: Vec<(f64, f64)>,
    /// 475th order statistic of `D*_m` (⌈0.95 (B+1)⌉).
    pub critical: f64,
    /// Only when `k == m`.
    pub p: Option<f64>,
    pub draws: usize,
    pub block: usize,
    pub breaks_crossed: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResidualChecks {
    pub key: u64,
    pub score_key: u64,
    pub model_digest: u64,
    pub adf: CheckState<AdfResult>,
    pub monitor: CheckState<MonitorResult>,
    pub half_life: CheckState<HalfLife>,
}

/// `Stationarity check pending: 23/60 scored bars` — never "stationary".
pub fn adf_pending_text(have: usize, need: usize) -> String {
    format!("Stationarity check pending: {have}/{need} scored bars")
}

pub fn checks_key(fit: &FrozenFit, r: &ResidualReport) -> ReportKey {
    let mut h = Fnv64::default();
    h.write_u64(r.key);
    h.write_u64(fit.content_digest);
    h.write_u64(fit.monitoring_horizon as u64);
    h.write_u64(fit.bootstrap_block as u64);
    h.write_u64(DRAWS as u64);
    h.write_u64(MIN_ADF as u64);
    h.write_str(METHOD);
    let mut key = ReportKey::new("studio_resid_check_v1", &fit.model.symbol, fit.model.tf, &r.source, &[], METHOD);
    key.params = h.finish();
    key
}

/// `(critical, p)` of the fixed-horizon statistic `d` against the bootstrap `draws` of `D*_m`:
/// critical = the ⌈0.95 (B+1)⌉-th order statistic, `p = (1 + #{D* ≥ d})/(B+1)`.
pub fn fixed_horizon_verdict(d: f64, draws: &[f64]) -> (f64, f64) {
    let b = draws.len();
    let mut sorted = draws.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = (95 * (b + 1)).div_ceil(100).clamp(1, b.max(1)) - 1;
    let ge = draws.iter().filter(|x| **x >= d).count();
    (sorted[at], (1 + ge) as f64 / (b + 1) as f64)
}

/// `D_k` and the path for centred partial sums.
pub fn cusum_path(e: &[f64], mu: f64) -> (Vec<f64>, f64) {
    let mut s = 0.;
    let mut d = 0f64;
    let path: Vec<f64> = e
        .iter()
        .map(|v| {
            s += v - mu;
            d = d.max(s.abs());
            s
        })
        .collect();
    (path, d)
}

/// The fixed-horizon CUSUM monitor alone (also the perf entry point).
pub fn monitor_check(fit: &FrozenFit, r: &ResidualReport) -> CheckState<MonitorResult> {
    let m = fit.monitoring_horizon;
    if fit.train_residuals.len() < m.max(MIN_ADF) {
        return CheckState::Unavailable(format!(
            "fewer than max(m, {MIN_ADF}) = {} training residuals stored",
            m.max(MIN_ADF)
        ));
    }
    if let Some((_, InputCheck::NotCovered { live_first_ts })) = r.revision.first()
        && *live_first_ts > fit.trained_thru
    {
        return CheckState::Unavailable("live window starts after monitoring start".into());
    }
    let k = r.scored.len().min(m);
    if k == 0 {
        return CheckState::Pending { have: 0, need: m, why: "no scored bars after the embargo yet".into() };
    }
    let (path, d_k) = cusum_path(&r.scored[..k], fit.residual_mean);
    let n = fit.train_residuals.len();
    let mu = fit.residual_mean;
    let mut dstar: Vec<f64> = Vec::with_capacity(DRAWS);
    let mut per_j: Vec<Vec<f64>> = vec![Vec::with_capacity(DRAWS); m];
    crate::studio_inference::stationary_bootstrap_runs(
        n,
        DRAWS,
        fit.content_digest,
        &fit.train_breaks,
        fit.bootstrap_block,
        |runs| {
            let (mut j, mut s, mut d) = (0usize, 0., 0f64);
            'outer: for &(start, len) in runs {
                for q in start..start + len {
                    s += fit.train_residuals[q] - mu;
                    d = d.max(s.abs());
                    per_j[j].push(s);
                    j += 1;
                    if j == m {
                        break 'outer;
                    }
                }
            }
            dstar.push(d);
        },
    );
    let (lo, hi) = (((DRAWS - 1) as f64 * 0.025).round() as usize, ((DRAWS - 1) as f64 * 0.975).round() as usize);
    let envelope: Vec<(f64, f64)> = per_j
        .iter_mut()
        .map(|v| {
            v.sort_by(f64::total_cmp);
            (v[lo.min(v.len() - 1)], v[hi.min(v.len() - 1)])
        })
        .collect();
    let (critical, p) = fixed_horizon_verdict(d_k, &dstar);
    CheckState::Ready(MonitorResult {
        m,
        k,
        path,
        d_k,
        envelope,
        critical,
        p: (k == m).then_some(p),
        draws: DRAWS,
        block: fit.bootstrap_block,
        breaks_crossed: r.scored_breaks[1..k].iter().filter(|b| **b).count(),
    })
}

/// Latest maximal run of SCORED observations with no break inside it: `(start, len)`.
pub fn latest_run(breaks: &[bool]) -> (usize, usize) {
    let start = breaks.iter().rposition(|b| *b).unwrap_or(0);
    (start, breaks.len() - start)
}

/// ADF, CUSUM monitor and half-life on SCORED rows only. Training and embargo rows never count; the
/// stored training residuals are only resampled for the monitor's reference distribution.
pub fn checks(fit: &FrozenFit, r: &ResidualReport) -> ResidualChecks {
    let key = checks_key(fit, r);
    let wrap = |adf: CheckState<AdfResult>, monitor: CheckState<MonitorResult>, half_life: CheckState<HalfLife>| {
        ResidualChecks { key: key.params, score_key: r.key, model_digest: fit.content_digest, adf, monitor, half_life }
    };
    if let Some(reason) = &r.unavailable {
        let u = reason.clone();
        return wrap(
            CheckState::Unavailable(u.clone()),
            CheckState::Unavailable(u.clone()),
            CheckState::Unavailable(u),
        );
    }
    let (start, t) = latest_run(&r.scored_breaks);
    let (adf, half_life) = if t < MIN_ADF {
        let why = adf_pending_text(t, MIN_ADF);
        (
            CheckState::Pending { have: t, need: MIN_ADF, why: why.clone() },
            CheckState::Pending { have: t, need: MIN_ADF, why },
        )
    } else {
        let seg = &r.scored[start..];
        let window = (r.scored_ts[start], *r.scored_ts.last().unwrap());
        let adf = match crate::studio_stationarity::adf(seg) {
            Ok(mut a) => {
                a.window = window;
                CheckState::Ready(a)
            }
            Err(e) => CheckState::Unavailable(e),
        };
        let hl = match crate::studio_stationarity::half_life(seg) {
            Ok(h) => CheckState::Ready(h),
            Err(e) => CheckState::Unavailable(e),
        };
        (adf, hl)
    };
    wrap(adf, monitor_check(fit, r), half_life)
}

#[cfg(test)]
#[path = "studio_residual_tests.rs"]
mod tests;
