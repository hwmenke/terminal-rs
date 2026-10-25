//! Historical risk/return statistics and Whats-News-style conditional distributions.
//!
//! Conceptual port of Whats-News `stats.py` and `conditional_dist.py`. Implemented in
//! Rust against verified daily bars from the local archive / Whats-News bridge.
//! Sample, unverified or mixed series are refused — never a silent fallback.
//!
//! Forward returns only count **completed** outcomes (`close[t+h] / close[t] − 1`).
//! Overlapping windows share future days; `effective_n = n / h` is the non-overlapping
//! equivalent so the raw count is not over-read.

use chrono::{Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tr_core::{
    Bar, Provenance,
    indicators::{ema, kama, macd, rsi, sma},
};

pub const DEFAULT_HORIZONS: [usize; 2] = [5, 10];
pub const HIST_BINS: usize = 30;
pub const DECILES: usize = 10;
pub const KAMA_PERIODS: [usize; 3] = [10, 20, 50];

const OPS: &[&str] = &["<=", ">=", "<", ">"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionError(pub String);

impl std::fmt::Display for ConditionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ConditionError {}

/// Comparison used by conditional distributions (`RSI(14) < 30`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cmp {
    pub left: String,
    pub op: String,
    pub right: String,
}

impl Cmp {
    pub fn label(&self) -> String {
        format!("{} {} {}", self.left, self.op, self.right)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RiskMetrics {
    pub n: usize,
    pub volatility: Option<f64>,
    pub sharpe: Option<f64>,
    pub max_drawdown: Option<f64>,
    pub avg_daily_ret: Option<f64>,
    pub win_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistBin {
    pub center: f64,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BinMean {
    pub bin: usize,
    pub value: Option<f64>,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonthMean {
    pub month: u32,
    pub value: Option<f64>,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossSummary {
    pub period: usize,
    pub direction: String,
    pub label: String,
    pub fwd_1d: Option<f64>,
    pub count_1d: usize,
    pub fwd_5d: Option<f64>,
    pub count_5d: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReturnStats {
    pub count: usize,
    /// `count / horizon` — overlapping forward windows share future days.
    pub effective_count: f64,
    pub mean: Option<f64>,
    pub median: Option<f64>,
    pub std: Option<f64>,
    pub skew: Option<f64>,
    pub kurtosis: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub p05: Option<f64>,
    pub p25: Option<f64>,
    pub p50: Option<f64>,
    pub p75: Option<f64>,
    pub p95: Option<f64>,
    pub win_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedHist {
    pub centers: Vec<f64>,
    pub conditional: Vec<usize>,
    pub baseline: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HorizonCompare {
    pub horizon: usize,
    pub conditional: ReturnStats,
    pub baseline: ReturnStats,
    pub hist: SharedHist,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatsReport {
    pub symbol: String,
    pub ticker: String,
    pub provenance: Provenance,
    pub source: String,
    pub as_of: String,
    pub start: String,
    pub end: String,
    pub bars: usize,
    pub metrics: RiskMetrics,
    pub distribution: Vec<HistBin>,
    pub rsi_fwd_1d: Vec<BinMean>,
    pub rsi_fwd_5d: Vec<BinMean>,
    pub vol_fwd_1d: Vec<BinMean>,
    pub kama_distance_1d: Vec<(usize, Vec<BinMean>)>,
    pub kama_distance_5d: Vec<(usize, Vec<BinMean>)>,
    pub kama_crosses: Vec<CrossSummary>,
    pub seasonality: Vec<MonthMean>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionalReport {
    pub symbol: String,
    pub ticker: String,
    pub provenance: Provenance,
    pub source: String,
    pub as_of: String,
    pub start: String,
    pub end: String,
    pub bars: usize,
    pub conditions: Vec<Cmp>,
    pub condition_label: String,
    pub match_count: usize,
    pub match_rate: f64,
    pub horizons: Vec<usize>,
    pub by_horizon: Vec<HorizonCompare>,
    pub overlap_note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchSnapshot {
    pub stats: StatsReport,
    pub conditional: ConditionalReport,
}

/// Shared gate for statistics and conditional returns. Sample/demo bars never compute.
pub fn require_research_bars(bars: &[Bar], operation: &str) -> Result<(), String> {
    if bars.is_empty() {
        return Err(format!(
            "{operation} refused: no daily history in the research archive. Sample or display prices are never used."
        ));
    }
    Bar::require_verified(bars, operation)?;
    if bars.windows(2).any(|w| w[0].ts >= w[1].ts) {
        return Err(format!("{operation} refused: daily bars must have unique ascending dates"));
    }
    if bars.iter().any(|b| {
        ![b.open, b.high, b.low, b.close, b.volume].iter().all(|v| v.is_finite())
            || b.close <= 0.0
            || b.open <= 0.0
            || b.low <= 0.0
    }) {
        return Err(format!("{operation} refused: invalid archived OHLCV"));
    }
    Ok(())
}

pub fn bar_date(ts: i64) -> String {
    Utc.timestamp_opt(ts, 0).single().unwrap_or_default().format("%Y-%m-%d").to_string()
}

pub fn parse_conditions(line: &str) -> Result<Vec<Cmp>, ConditionError> {
    let raw = line.trim();
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let chunks: Vec<String> = split_and_clauses(raw);
    if chunks.is_empty() {
        return parse_one(raw).map(|c| vec![c]);
    }
    let mut out = Vec::new();
    for chunk in &chunks {
        if let Ok(c) = parse_one(chunk) {
            out.push(c);
            continue;
        }
        // Implicit AND: `RSI < 30 PRICE > SMA(50)`
        let mut rest = chunk.clone();
        let mut found = false;
        while !rest.trim().is_empty() {
            match next_cmp(&rest) {
                Some((c, leftover)) => {
                    out.push(c);
                    rest = leftover;
                    found = true;
                }
                None => {
                    if found {
                        return Err(ConditionError(format!("Unrecognized condition: {rest}")));
                    }
                    return Err(ConditionError(format!("Condition needs left, op and right: {chunk}")));
                }
            }
        }
    }
    if out.is_empty() {
        return Err(ConditionError(format!("Condition needs left, op and right: {raw}")));
    }
    Ok(out)
}

fn parse_one(s: &str) -> Result<Cmp, ConditionError> {
    let s = s.trim();
    for op in OPS {
        if let Some(at) = find_op(s, op) {
            let left = s[..at].trim();
            let right = s[at + op.len()..].trim();
            if left.is_empty() || right.is_empty() {
                return Err(ConditionError(format!("Condition needs left, op and right: {s}")));
            }
            return Ok(Cmp { left: normalize_feature(left)?, op: (*op).into(), right: normalize_operand(right)? });
        }
    }
    Err(ConditionError(format!("Unsupported operator in {s:?} (use <, <=, >, >=)")))
}

fn next_cmp(s: &str) -> Option<(Cmp, String)> {
    let s = s.trim();
    let mut best: Option<(usize, &'static str)> = None;
    for op in OPS {
        if let Some(at) = find_op(s, op) {
            if best.is_none_or(|(b, _)| at < b) {
                best = Some((at, *op));
            }
        }
    }
    let (at, op) = best?;
    let left = s[..at].trim();
    if left.is_empty() {
        return None;
    }
    let after = s[at + op.len()..].trim_start();
    let (right, leftover) = split_operand(after)?;
    let left = normalize_feature(left).ok()?;
    let right = normalize_operand(&right).ok()?;
    Some((Cmp { left, op: op.into(), right }, leftover))
}

fn split_and_clauses(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut rest = raw;
    while !rest.is_empty() {
        if rest.starts_with(',') || rest.starts_with(';') {
            push_clause(&mut out, &mut buf);
            rest = &rest[1..];
            continue;
        }
        if rest.len() >= 5 && rest[..5].eq_ignore_ascii_case(" and ") {
            push_clause(&mut out, &mut buf);
            rest = &rest[5..];
            continue;
        }
        let ch = rest.chars().next().unwrap();
        buf.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    push_clause(&mut out, &mut buf);
    out
}

fn push_clause(out: &mut Vec<String>, buf: &mut String) {
    let s = buf.trim();
    if !s.is_empty() {
        out.push(s.to_string());
    }
    buf.clear();
}

fn find_op(s: &str, op: &str) -> Option<usize> {
    let mut i = 0;
    while i < s.len() {
        if s.is_char_boundary(i) && s[i..].starts_with(op) {
            let before = s[..i].chars().next_back();
            let after = s.get(i + op.len()..).and_then(|t| t.chars().next());
            let edge = |c: Option<char>| {
                c.is_none_or(|c| c.is_whitespace() || c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ')'))
            };
            if edge(before) && edge(after) {
                if op.len() == 1 && s[i..].starts_with(&format!("{op}=")) {
                    i += 1;
                    continue;
                }
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn split_operand(s: &str) -> Option<(String, String)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    // `SMA(50)` or `30` or `-1.5`
    if looks_complete(tokens[0]) {
        return Some((tokens[0].to_string(), tokens.get(1..).unwrap_or(&[]).join(" ")));
    }
    // `SMA 50` consumes two tokens.
    if tokens.len() >= 2 && is_feature_name(tokens[0]) && tokens[1].parse::<f64>().is_ok() {
        return Some((format!("{} {}", tokens[0], tokens[1]), tokens.get(2..).unwrap_or(&[]).join(" ")));
    }
    Some((tokens[0].to_string(), tokens.get(1..).unwrap_or(&[]).join(" ")))
}

fn looks_complete(tok: &str) -> bool {
    tok.parse::<f64>().is_ok()
        || tok.contains('(')
        || tok.eq_ignore_ascii_case("price")
        || tok.eq_ignore_ascii_case("close")
        || tok.eq_ignore_ascii_case("open")
        || tok.eq_ignore_ascii_case("high")
        || tok.eq_ignore_ascii_case("low")
        || tok.eq_ignore_ascii_case("volume")
        || tok.eq_ignore_ascii_case("macd")
        || tok.eq_ignore_ascii_case("macd_signal")
        || tok.eq_ignore_ascii_case("macd_hist")
}

fn is_feature_name(tok: &str) -> bool {
    matches!(tok.to_ascii_lowercase().as_str(), "rsi" | "ma" | "sma" | "ema" | "roc" | "kama")
}

fn normalize_feature(raw: &str) -> Result<String, ConditionError> {
    normalize_operand(raw)
}

fn normalize_operand(raw: &str) -> Result<String, ConditionError> {
    let t = raw.trim();
    if t.parse::<f64>().is_ok() {
        return Ok(t.to_string());
    }
    let lower = t.to_ascii_lowercase().replace(' ', "");
    if lower.eq_ignore_ascii_case("price") || lower == "c" || lower == "close" {
        return Ok("close".into());
    }
    if matches!(lower.as_str(), "open" | "high" | "low" | "volume") {
        return Ok(lower);
    }
    if lower == "macd" || lower == "macd_line" {
        return Ok("macd".into());
    }
    if lower == "macd_signal" {
        return Ok("macd_signal".into());
    }
    if lower == "macd_hist" {
        return Ok("macd_hist".into());
    }
    // `RSI(14)` / `RSI14` / `SMA 50`
    let parts: Vec<&str> = t.split_whitespace().collect();
    let (name, period) = if parts.len() == 2 && parts[1].parse::<usize>().is_ok() {
        (parts[0], Some(parts[1].parse::<usize>().unwrap()))
    } else if let Some((n, p)) = split_name_period(t) {
        (n, p)
    } else if is_feature_name(t) {
        (t, None)
    } else {
        return Err(ConditionError(format!("Unrecognized feature: {raw}")));
    };
    let key = name.to_ascii_lowercase();
    match key.as_str() {
        "rsi" => Ok(format!("RSI({})", period.unwrap_or(14))),
        "ma" | "sma" => {
            let p = period.ok_or_else(|| ConditionError("Moving average needs a period, e.g. SMA(50)".into()))?;
            Ok(format!("SMA({p})"))
        }
        "ema" => {
            let p = period.ok_or_else(|| ConditionError("EMA needs a period, e.g. EMA(20)".into()))?;
            Ok(format!("EMA({p})"))
        }
        "roc" => {
            let p = period.ok_or_else(|| ConditionError("ROC needs a period, e.g. ROC(20)".into()))?;
            Ok(format!("ROC({p})"))
        }
        "kama" => {
            let p = period.ok_or_else(|| ConditionError("KAMA needs a period, e.g. KAMA(10)".into()))?;
            Ok(format!("KAMA({p})"))
        }
        _ => Err(ConditionError(format!("Unknown feature: {raw}"))),
    }
}

fn split_name_period(s: &str) -> Option<(&str, Option<usize>)> {
    let s = s.trim();
    if let Some(open) = s.find('(') {
        if !s.ends_with(')') {
            return None;
        }
        let name = s[..open].trim();
        let inner = s[open + 1..s.len() - 1].trim();
        if inner.is_empty() {
            return Some((name, None));
        }
        return Some((name, Some(inner.parse().ok()?)));
    }
    let split = s.chars().position(|c| c.is_ascii_digit())?;
    if split == 0 {
        return None;
    }
    let (name, num) = s.split_at(split);
    Some((name, Some(num.parse().ok()?)))
}

fn feature_series(name: &str, bars: &[Bar]) -> Result<Vec<f64>, ConditionError> {
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let key = name.trim();
    let low = key.to_ascii_lowercase();
    if low == "close" || low == "price" || low == "c" {
        return Ok(closes);
    }
    if low == "open" {
        return Ok(bars.iter().map(|b| b.open).collect());
    }
    if low == "high" {
        return Ok(bars.iter().map(|b| b.high).collect());
    }
    if low == "low" {
        return Ok(bars.iter().map(|b| b.low).collect());
    }
    if low == "volume" {
        return Ok(bars.iter().map(|b| b.volume).collect());
    }
    if low == "macd" {
        return Ok(macd(&closes, 12, 26, 9).macd);
    }
    if low == "macd_signal" {
        return Ok(macd(&closes, 12, 26, 9).signal);
    }
    if low == "macd_hist" {
        return Ok(macd(&closes, 12, 26, 9).histogram);
    }
    let (base, period) =
        split_name_period(key).ok_or_else(|| ConditionError(format!("Unrecognized feature: {name}")))?;
    match base.to_ascii_lowercase().as_str() {
        "rsi" => Ok(rsi(&closes, period.unwrap_or(14))),
        "sma" | "ma" => {
            let p = period.ok_or_else(|| ConditionError("Moving average needs a period, e.g. SMA(50)".into()))?;
            Ok(sma(&closes, p))
        }
        "ema" => {
            let p = period.ok_or_else(|| ConditionError("EMA needs a period, e.g. EMA(20)".into()))?;
            Ok(ema(&closes, p))
        }
        "roc" => {
            let p = period.ok_or_else(|| ConditionError("ROC needs a period, e.g. ROC(20)".into()))?;
            Ok(roc(&closes, p))
        }
        "kama" => {
            let p = period.ok_or_else(|| ConditionError("KAMA needs a period, e.g. KAMA(10)".into()))?;
            Ok(kama(&closes, p, 2, 30))
        }
        _ => Err(ConditionError(format!("Unknown feature: {name}"))),
    }
}

fn roc(closes: &[f64], period: usize) -> Vec<f64> {
    (0..closes.len())
        .map(|i| {
            if i >= period && closes[i].is_finite() && closes[i - period].is_finite() && closes[i - period] != 0.0 {
                (closes[i] / closes[i - period] - 1.0) * 100.0
            } else {
                f64::NAN
            }
        })
        .collect()
}

fn operand_series(raw: &str, bars: &[Bar]) -> Result<Vec<f64>, ConditionError> {
    if let Ok(n) = raw.trim().parse::<f64>() {
        return Ok(vec![n; bars.len()]);
    }
    feature_series(raw, bars)
}

fn apply_op(op: &str, left: f64, right: f64) -> bool {
    if !left.is_finite() || !right.is_finite() {
        return false;
    }
    match op {
        "<" => left < right,
        "<=" => left <= right,
        ">" => left > right,
        ">=" => left >= right,
        _ => false,
    }
}

pub fn match_mask(bars: &[Bar], conditions: &[Cmp]) -> Result<Vec<bool>, ConditionError> {
    let mut mask = vec![true; bars.len()];
    for c in conditions {
        let left = operand_series(&c.left, bars)?;
        let right = operand_series(&c.right, bars)?;
        for i in 0..bars.len() {
            mask[i] = mask[i] && apply_op(&c.op, left[i], right[i]);
        }
    }
    Ok(mask)
}

/// Forward close-to-close return over `h` completed bars. Last `h` values are None.
pub fn forward_returns(closes: &[f64], h: usize) -> Vec<Option<f64>> {
    if h == 0 {
        return vec![None; closes.len()];
    }
    (0..closes.len())
        .map(|i| {
            let j = i + h;
            if j < closes.len() && closes[i] > 0.0 && closes[j].is_finite() && closes[i].is_finite() {
                Some(closes[j] / closes[i] - 1.0)
            } else {
                None
            }
        })
        .collect()
}

fn describe(values: &[f64], horizon: usize) -> ReturnStats {
    let empty = ReturnStats {
        count: 0,
        effective_count: 0.0,
        mean: None,
        median: None,
        std: None,
        skew: None,
        kurtosis: None,
        min: None,
        max: None,
        p05: None,
        p25: None,
        p50: None,
        p75: None,
        p95: None,
        win_rate: None,
    };
    if values.is_empty() {
        return empty;
    }
    let mut xs = values.to_vec();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = xs.len();
    let mean = xs.iter().sum::<f64>() / n as f64;
    let median = percentile(&xs, 50.0);
    let std = if n > 1 {
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0);
        Some(var.max(0.0).sqrt())
    } else {
        None
    };
    ReturnStats {
        count: n,
        effective_count: if horizon == 0 { n as f64 } else { n as f64 / horizon as f64 },
        mean: Some(mean),
        median: Some(median),
        std,
        skew: moments_skew(&xs, mean, n),
        kurtosis: moments_kurt(&xs, mean, n),
        min: xs.first().copied(),
        max: xs.last().copied(),
        p05: Some(percentile(&xs, 5.0)),
        p25: Some(percentile(&xs, 25.0)),
        p50: Some(percentile(&xs, 50.0)),
        p75: Some(percentile(&xs, 75.0)),
        p95: Some(percentile(&xs, 95.0)),
        win_rate: Some(xs.iter().filter(|x| **x > 0.0).count() as f64 / n as f64),
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = (p / 100.0) * (sorted.len() as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let w = rank - lo as f64;
        sorted[lo] * (1.0 - w) + sorted[hi] * w
    }
}

fn moments_skew(xs: &[f64], mean: f64, n: usize) -> Option<f64> {
    if n < 3 {
        return None;
    }
    let m2 = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
    let m3 = xs.iter().map(|x| (x - mean).powi(3)).sum::<f64>() / n as f64;
    if m2 <= 0.0 {
        return Some(0.0);
    }
    let g1 = m3 / m2.powf(1.5);
    Some((n as f64 * (n as f64 - 1.0)).sqrt() / (n as f64 - 2.0) * g1)
}

fn moments_kurt(xs: &[f64], mean: f64, n: usize) -> Option<f64> {
    if n < 4 {
        return None;
    }
    let m2 = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
    let m4 = xs.iter().map(|x| (x - mean).powi(4)).sum::<f64>() / n as f64;
    if m2 <= 0.0 {
        return Some(0.0);
    }
    let g2 = m4 / m2.powi(2) - 3.0;
    let nf = n as f64;
    Some(((nf - 1.0) / ((nf - 2.0) * (nf - 3.0))) * ((nf + 1.0) * g2 + 6.0))
}

fn shared_hist(cond: &[f64], base: &[f64], bins: usize) -> SharedHist {
    if base.is_empty() || bins == 0 {
        return SharedHist { centers: vec![], conditional: vec![], baseline: vec![] };
    }
    let lo = base.iter().copied().fold(f64::INFINITY, f64::min);
    let mut hi = base.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if lo == hi {
        hi = lo + 1e-9;
    }
    let step = (hi - lo) / bins as f64;
    let mut c_counts = vec![0usize; bins];
    let mut b_counts = vec![0usize; bins];
    let place = |v: f64, counts: &mut [usize]| {
        if !v.is_finite() {
            return;
        }
        let mut i = ((v - lo) / step).floor() as isize;
        if i < 0 {
            i = 0;
        }
        if i as usize >= bins {
            i = bins as isize - 1;
        }
        counts[i as usize] += 1;
    };
    for &v in cond {
        place(v, &mut c_counts);
    }
    for &v in base {
        place(v, &mut b_counts);
    }
    let centers = (0..bins).map(|i| lo + step * (i as f64 + 0.5)).collect();
    SharedHist { centers, conditional: c_counts, baseline: b_counts }
}

fn qcut_means(feature: &[f64], target: &[Option<f64>], bins: usize) -> Vec<BinMean> {
    let mut pairs: Vec<(f64, f64)> = feature
        .iter()
        .zip(target)
        .filter_map(|(&f, t)| if f.is_finite() { t.filter(|v| v.is_finite()).map(|v| (f, v)) } else { None })
        .collect();
    if pairs.len() < bins || bins == 0 {
        return Vec::new();
    }
    pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut edges = Vec::with_capacity(bins + 1);
    for i in 0..=bins {
        let rank = (i as f64 / bins as f64) * (pairs.len() as f64 - 1.0);
        edges.push(pairs[rank.round() as usize].0);
    }
    edges.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
    if edges.len() < 2 {
        return Vec::new();
    }
    let groups = edges.len() - 1;
    let mut sums = vec![0.0; groups];
    let mut counts = vec![0usize; groups];
    for (f, t) in pairs {
        let mut g = edges.partition_point(|e| *e <= f).saturating_sub(1);
        if g >= groups {
            g = groups - 1;
        }
        sums[g] += t;
        counts[g] += 1;
    }
    counts
        .iter()
        .enumerate()
        .filter(|(_, c)| **c > 0)
        .map(|(i, c)| BinMean { bin: i, value: Some(sums[i] / *c as f64), count: *c })
        .collect()
}

fn daily_returns(bars: &[Bar]) -> Vec<Option<f64>> {
    (0..bars.len())
        .map(|i| if i == 0 || bars[i - 1].close <= 0.0 { None } else { Some(bars[i].close / bars[i - 1].close - 1.0) })
        .collect()
}

fn risk_metrics(rets: &[f64]) -> RiskMetrics {
    if rets.is_empty() {
        return RiskMetrics {
            n: 0,
            volatility: None,
            sharpe: None,
            max_drawdown: None,
            avg_daily_ret: None,
            win_rate: None,
        };
    }
    let n = rets.len();
    let mean = rets.iter().sum::<f64>() / n as f64;
    let std = if n > 1 {
        let var = rets.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0);
        var.max(0.0).sqrt()
    } else {
        0.0
    };
    let vol = std * 252.0_f64.sqrt();
    let sharpe = if std > 0.0 { Some(mean / std * 252.0_f64.sqrt()) } else { None };
    let mut equity = 1.0_f64;
    let mut peak = 1.0_f64;
    let mut max_dd = 0.0_f64;
    for r in rets {
        equity *= 1.0 + r;
        peak = peak.max(equity);
        if peak > 0.0 {
            max_dd = max_dd.min(equity / peak - 1.0);
        }
    }
    RiskMetrics {
        n,
        volatility: Some(vol),
        sharpe,
        max_drawdown: Some(max_dd),
        avg_daily_ret: Some(mean),
        win_rate: Some(rets.iter().filter(|r| **r > 0.0).count() as f64 / n as f64),
    }
}

fn hist_of(values: &[f64], bins: usize) -> Vec<HistBin> {
    if values.is_empty() || bins == 0 {
        return Vec::new();
    }
    let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
    let mut hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if lo == hi {
        hi = lo + 1e-9;
    }
    let step = (hi - lo) / bins as f64;
    let mut counts = vec![0usize; bins];
    for &v in values {
        let mut i = ((v - lo) / step).floor() as isize;
        i = i.clamp(0, bins as isize - 1);
        counts[i as usize] += 1;
    }
    counts.into_iter().enumerate().map(|(i, count)| HistBin { center: lo + step * (i as f64 + 0.5), count }).collect()
}

fn seasonality(bars: &[Bar], daily: &[Option<f64>]) -> Vec<MonthMean> {
    let mut sums = [0.0; 13];
    let mut counts = [0usize; 13];
    for (b, r) in bars.iter().zip(daily) {
        let Some(r) = r.filter(|v| v.is_finite()) else { continue };
        let month = Utc.timestamp_opt(b.ts, 0).single().map(|d| d.month()).unwrap_or(0);
        if (1..=12).contains(&month) {
            sums[month as usize] += r;
            counts[month as usize] += 1;
        }
    }
    (1..=12)
        .filter(|&m| counts[m] > 0)
        .map(|m| MonthMean { month: m as u32, value: Some(sums[m] / counts[m] as f64 * 21.0), count: counts[m] })
        .collect()
}

/// Historical risk, return distribution, RSI/vol/KAMA factor bins and seasonality.
pub fn compute_stats(
    symbol: &str,
    ticker: &str,
    bars: &[Bar],
    source: &str,
    operation: &str,
) -> Result<StatsReport, String> {
    require_research_bars(bars, operation)?;
    let provenance = Bar::provenance(bars);
    provenance.require_verified(operation)?;
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let daily = daily_returns(bars);
    let rets: Vec<f64> = daily.iter().copied().flatten().filter(|v| v.is_finite()).collect();
    let fwd_1d = forward_returns(&closes, 1);
    let fwd_5d = forward_returns(&closes, 5);
    let rsi14 = rsi(&closes, 14);
    let vol20: Vec<f64> = {
        let mut out = vec![f64::NAN; bars.len()];
        for i in 20..bars.len() {
            let w: Vec<f64> = daily[i + 1 - 20..=i].iter().copied().flatten().collect();
            if w.len() == 20 {
                let m = w.iter().sum::<f64>() / 20.0;
                let var = w.iter().map(|x| (x - m).powi(2)).sum::<f64>() / 20.0;
                out[i] = var.max(0.0).sqrt() * 252.0_f64.sqrt();
            }
        }
        out
    };
    let mut kama_distance_1d = Vec::new();
    let mut kama_distance_5d = Vec::new();
    let mut kama_crosses = Vec::new();
    for period in KAMA_PERIODS {
        let k = kama(&closes, period, 2, 30);
        let gap: Vec<f64> = closes
            .iter()
            .zip(&k)
            .map(|(c, ka)| if ka.is_finite() && *ka != 0.0 && c.is_finite() { c / ka - 1.0 } else { f64::NAN })
            .collect();
        kama_distance_1d.push((period, qcut_means(&gap, &fwd_1d, DECILES)));
        kama_distance_5d.push((period, qcut_means(&gap, &fwd_5d, DECILES)));
        for (direction, bull) in [("bull", true), ("bear", false)] {
            let mut r1 = Vec::new();
            let mut r5 = Vec::new();
            for i in 1..bars.len() {
                if !gap[i].is_finite() || !gap[i - 1].is_finite() {
                    continue;
                }
                let hit = if bull { gap[i - 1] <= 0.0 && gap[i] > 0.0 } else { gap[i - 1] >= 0.0 && gap[i] < 0.0 };
                if hit {
                    if let Some(v) = fwd_1d[i] {
                        r1.push(v);
                    }
                    if let Some(v) = fwd_5d[i] {
                        r5.push(v);
                    }
                }
            }
            kama_crosses.push(CrossSummary {
                period,
                direction: direction.into(),
                label: format!("K{period} {}", if bull { "Bull" } else { "Bear" }),
                fwd_1d: mean_of(&r1),
                count_1d: r1.len(),
                fwd_5d: mean_of(&r5),
                count_5d: r5.len(),
            });
        }
    }
    Ok(StatsReport {
        symbol: symbol.into(),
        ticker: ticker.into(),
        provenance,
        source: provenance.source_label(source).into(),
        as_of: bars.last().map(|b| bar_date(b.ts)).unwrap_or_default(),
        start: bars.first().map(|b| bar_date(b.ts)).unwrap_or_default(),
        end: bars.last().map(|b| bar_date(b.ts)).unwrap_or_default(),
        bars: bars.len(),
        metrics: risk_metrics(&rets),
        distribution: hist_of(&rets, HIST_BINS),
        rsi_fwd_1d: qcut_means(&rsi14, &fwd_1d, DECILES),
        rsi_fwd_5d: qcut_means(&rsi14, &fwd_5d, DECILES),
        vol_fwd_1d: qcut_means(&vol20, &fwd_1d, DECILES),
        kama_distance_1d,
        kama_distance_5d,
        kama_crosses,
        seasonality: seasonality(bars, &daily),
    })
}

fn mean_of(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() { None } else { Some(xs.iter().sum::<f64>() / xs.len() as f64) }
}

/// Conditional vs baseline forward-return distributions. Empty conditions = baseline only.
pub fn compute_conditional(
    symbol: &str,
    ticker: &str,
    bars: &[Bar],
    conditions: &[Cmp],
    horizons: &[usize],
    source: &str,
    operation: &str,
) -> Result<ConditionalReport, String> {
    require_research_bars(bars, operation)?;
    let provenance = Bar::provenance(bars);
    provenance.require_verified(operation)?;
    let mask = match_mask(bars, conditions).map_err(|e| e.0)?;
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let match_count = mask.iter().filter(|m| **m).count();
    let horizons: Vec<usize> = if horizons.is_empty() { DEFAULT_HORIZONS.to_vec() } else { horizons.to_vec() };
    let mut by_horizon = Vec::new();
    for &h in &horizons {
        let fwd = forward_returns(&closes, h);
        let cond: Vec<f64> = fwd.iter().enumerate().filter_map(|(i, v)| if mask[i] { *v } else { None }).collect();
        let base: Vec<f64> = fwd.iter().copied().flatten().collect();
        by_horizon.push(HorizonCompare {
            horizon: h,
            conditional: describe(&cond, h),
            baseline: describe(&base, h),
            hist: shared_hist(&cond, &base, HIST_BINS),
        });
    }
    let label = if conditions.is_empty() {
        "unconditional (every completed bar)".into()
    } else {
        conditions.iter().map(|c| c.label()).collect::<Vec<_>>().join(" AND ")
    };
    Ok(ConditionalReport {
        symbol: symbol.into(),
        ticker: ticker.into(),
        provenance,
        source: provenance.source_label(source).into(),
        as_of: bars.last().map(|b| bar_date(b.ts)).unwrap_or_default(),
        start: bars.first().map(|b| bar_date(b.ts)).unwrap_or_default(),
        end: bars.last().map(|b| bar_date(b.ts)).unwrap_or_default(),
        bars: bars.len(),
        conditions: conditions.to_vec(),
        condition_label: label,
        match_count,
        match_rate: if bars.is_empty() { 0.0 } else { match_count as f64 / bars.len() as f64 },
        horizons: horizons.clone(),
        by_horizon,
        overlap_note: "N is completed outcomes only (last H bars have no close yet). Overlapping H-day windows share future days, so effective N = N / H is the non-overlapping equivalent.".into(),
    })
}

pub fn compute_snapshot(
    symbol: &str,
    ticker: &str,
    bars: &[Bar],
    condition_line: &str,
    horizons: &[usize],
    source: &str,
) -> Result<ResearchSnapshot, String> {
    let conditions = parse_conditions(condition_line).map_err(|e| e.0)?;
    Ok(ResearchSnapshot {
        stats: compute_stats(symbol, ticker, bars, source, "Statistics")?,
        conditional: compute_conditional(symbol, ticker, bars, &conditions, horizons, source, "Conditional returns")?,
    })
}

/// Built-in condition chips shown in the pane.
pub fn presets() -> &'static [(&'static str, &'static str)] {
    &[
        ("RSI OS", "RSI(14) < 30"),
        ("RSI OB", "RSI(14) > 70"),
        ("above SMA20", "close > SMA(20)"),
        ("above SMA50", "close > SMA(50)"),
        ("below SMA20", "close < SMA(20)"),
        ("SMA20 > SMA50", "SMA(20) > SMA(50)"),
        ("RSI OS + SMA50", "RSI(14) < 30 AND close > SMA(50)"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(i: i64) -> i64 {
        1_700_000_000 + i * 86_400
    }

    fn real(ts: i64, close: f64) -> Bar {
        Bar::new(ts, close, close + 1.0, close - 1.0, close, 1_000.0).with_provenance(Provenance::Real)
    }

    fn series(n: usize, mut f: impl FnMut(usize) -> f64) -> Vec<Bar> {
        (0..n).map(|i| real(day(i as i64), f(i))).collect()
    }

    #[test]
    fn sample_and_unverified_bars_never_enter_stats() {
        let sample = vec![Bar::new(day(0), 1.0, 1.0, 1.0, 1.0, 1.0).with_provenance(Provenance::Sample)];
        let unverified = vec![Bar::new(day(0), 1.0, 1.0, 1.0, 1.0, 1.0)];
        let mixed = vec![real(day(0), 10.0), sample[0]];
        for (bars, needle) in [(&sample, "SAMPLE"), (&unverified, "UNVERIFIED"), (&mixed, "SAMPLE")] {
            let err = compute_stats("AAPL US Equity", "AAPL", bars, "test", "Statistics").unwrap_err();
            assert!(err.contains(needle), "{err}");
            let err = compute_conditional("X", "X", bars, &[], &[5], "test", "Conditional returns").unwrap_err();
            assert!(err.contains(needle), "{err}");
        }
        assert!(require_research_bars(&[], "Statistics").unwrap_err().contains("no daily history"));
    }

    #[test]
    fn completed_outcomes_exclude_unfinished_horizon() {
        let bars = series(20, |i| 100.0 + i as f64);
        let fwd = forward_returns(&bars.iter().map(|b| b.close).collect::<Vec<_>>(), 5);
        assert_eq!(fwd.iter().filter(|v| v.is_some()).count(), 15);
        assert!(fwd[15..].iter().all(|v| v.is_none()));
        assert!((fwd[0].unwrap() - 0.05).abs() < 1e-9);
        let report = compute_conditional("X", "X", &bars, &[], &[5], "unit test", "Conditional returns").unwrap();
        assert_eq!(report.start, bar_date(bars[0].ts));
        assert_eq!(report.end, bar_date(bars[19].ts));
        assert_eq!(report.as_of, report.end);
        assert_eq!(report.by_horizon[0].baseline.count, 15);
        assert!((report.by_horizon[0].baseline.effective_count - 3.0).abs() < 1e-9);
        assert!(report.overlap_note.contains("effective N"));
        assert_eq!(report.provenance, Provenance::Real);
        assert_eq!(report.source, "unit test");
    }

    #[test]
    fn overlapping_windows_are_labelled_not_silently_deflated() {
        let bars = series(12, |i| 10.0 + i as f64);
        let report = compute_conditional("X", "X", &bars, &[], &[5], "t", "Conditional returns").unwrap();
        let h = &report.by_horizon[0];
        assert_eq!(h.conditional.count, 7);
        assert!((h.conditional.effective_count - 7.0 / 5.0).abs() < 1e-9);
        assert_eq!(h.conditional.count, h.baseline.count);
    }

    #[test]
    fn low_rsi_condition_selects_a_subset_against_baseline() {
        // Long decline then recovery so RSI spends time below 30.
        let mut px = 100.0;
        let bars = series(80, |i| {
            if i < 40 {
                px *= 0.97;
            } else {
                px *= 1.02;
            }
            px
        });
        let conds = parse_conditions("RSI(14) < 30").unwrap();
        assert_eq!(conds[0].label(), "RSI(14) < 30");
        let report = compute_conditional("X", "X", &bars, &conds, &[5, 10], "t", "Conditional returns").unwrap();
        assert!(report.match_count > 0);
        assert!(report.match_count < bars.len());
        assert_eq!(report.by_horizon.len(), 2);
        let h5 = &report.by_horizon[0];
        assert!(h5.conditional.count < h5.baseline.count);
        assert!(h5.conditional.count > 0);
        assert_eq!(h5.hist.centers.len(), HIST_BINS);
        assert_eq!(h5.hist.baseline.iter().sum::<usize>(), h5.baseline.count);
        assert_eq!(h5.hist.conditional.iter().sum::<usize>(), h5.conditional.count);
        assert!(h5.baseline.mean.is_some());
        assert!(report.condition_label.contains("RSI(14) < 30"));
    }

    #[test]
    fn price_above_sma_and_and_clauses_parse() {
        let a = parse_conditions("PRICE > SMA(50)").unwrap();
        assert_eq!(a[0], Cmp { left: "close".into(), op: ">".into(), right: "SMA(50)".into() });
        let b = parse_conditions("RSI < 30 AND close > SMA 20").unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].left, "RSI(14)");
        assert_eq!(b[1].right, "SMA(20)");
        let c = parse_conditions("RSI(14) < 30 close > SMA(50)").unwrap();
        assert_eq!(c.len(), 2);
        assert!(parse_conditions("RSI ~= 30").is_err());
        assert!(parse_conditions("FOO(5) < 1").is_err());
    }

    #[test]
    fn stats_keep_provenance_and_date_range() {
        let bars = series(40, |i| 50.0 + (i as f64).sin() + i as f64 * 0.1);
        let report =
            compute_stats("AAPL US Equity", "AAPL", &bars, "Local Yahoo daily research archive", "Statistics").unwrap();
        assert_eq!(report.provenance, Provenance::Real);
        assert_eq!(report.source, "Local Yahoo daily research archive");
        assert_eq!(report.bars, 40);
        assert_eq!(report.start, bar_date(bars[0].ts));
        assert_eq!(report.end, bar_date(bars[39].ts));
        assert!(report.metrics.n > 0);
        assert!(report.metrics.volatility.unwrap() > 0.0);
        assert!(report.metrics.win_rate.unwrap() > 0.0);
        assert!(!report.distribution.is_empty());
        assert!(!report.seasonality.is_empty());
        assert_eq!(report.kama_crosses.len(), 6);
    }

    #[test]
    fn snapshot_refuses_to_mix_sample_into_outcomes() {
        let mut bars = series(30, |i| 20.0 + i as f64);
        bars[10].provenance = Provenance::Sample;
        assert!(compute_snapshot("X", "X", &bars, "RSI < 30", &[5], "t").unwrap_err().contains("SAMPLE"));
    }
}
