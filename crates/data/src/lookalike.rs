//! Historical lookalikes (nearest-neighbor analogs) on verified daily bars.
//!
//! Conceptual port of Whats-News `knn_model.py`. Implemented in Rust against
//! verified daily bars from the local archive / Whats-News bridge.
//! Sample, unverified or mixed series are refused — never a silent fallback.
//!
//! Neighbors are **past** sessions whose backward-looking feature vector is
//! close to the query date (standardized Euclidean distance). Forward returns
//! only count **completed** outcomes (`close[t+h] / close[t] − 1`). Incomplete
//! horizons are labelled, never invented. Overlapping neighbors are reduced by
//! a minimum session gap; requested K, selected N, discarded-overlap and
//! per-horizon completed / incomplete counts are always shown before frequencies.

use serde::{Deserialize, Serialize};
use tr_core::{
    Bar, Provenance,
    indicators::{rsi, sma},
};

use crate::journal;
use crate::stats::{bar_date, require_research_bars};

pub const DEFAULT_K: usize = 20;
pub const DEFAULT_HORIZONS: [usize; 3] = [5, 10, 21];
pub const DEFAULT_GAP: usize = 21;
pub const FEATURE_LOOKBACK: usize = 50;
pub const OPERATION: &str = "Lookalikes";

const FEATURE_NAMES: [&str; 10] = [
    "ret_1d",
    "ret_5d",
    "ret_10d",
    "ret_21d",
    "rsi_14",
    "vol_21",
    "dist_sma20",
    "dist_sma50",
    "volume_ratio",
    "range_21",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LookalikeQuery {
    pub k: usize,
    pub horizons: Vec<usize>,
    pub as_of: Option<String>,
    pub gap: usize,
}

impl Default for LookalikeQuery {
    fn default() -> Self {
        Self { k: DEFAULT_K, horizons: DEFAULT_HORIZONS.to_vec(), as_of: None, gap: DEFAULT_GAP }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureDef {
    pub name: String,
    pub lookback: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryContext {
    pub date: String,
    pub ts: i64,
    pub index: usize,
    pub features: Vec<f64>,
    pub z: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HorizonOutcome {
    pub horizon: usize,
    pub completed: bool,
    pub ret: Option<f64>,
    /// Present when the forward window has not finished (never a invented return).
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Neighbor {
    pub date: String,
    pub ts: i64,
    pub index: usize,
    pub distance: f64,
    pub features: Vec<f64>,
    pub outcomes: Vec<HorizonOutcome>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HorizonSummary {
    pub horizon: usize,
    pub requested_n: usize,
    pub completed_n: usize,
    pub incomplete_n: usize,
    /// Non-overlapping equivalent after the neighbor gap. Equals `completed_n`
    /// when `gap >= horizon`; otherwise `completed_n * gap / horizon`.
    pub effective_n: f64,
    pub overlap_note: String,
    pub mean: Option<f64>,
    pub median: Option<f64>,
    pub std: Option<f64>,
    pub win_rate: Option<f64>,
    pub p25: Option<f64>,
    pub p75: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LookalikeReport {
    pub symbol: String,
    pub ticker: String,
    pub provenance: Provenance,
    pub source: String,
    pub as_of: String,
    pub start: String,
    pub end: String,
    pub bars: usize,
    pub query: QueryContext,
    pub feature_names: Vec<String>,
    pub feature_note: String,
    pub distance: String,
    pub requested_k: usize,
    pub selected_n: usize,
    pub discarded_overlap: usize,
    pub eligible_n: usize,
    pub gap: usize,
    pub neighbors: Vec<Neighbor>,
    pub by_horizon: Vec<HorizonSummary>,
    pub overlap_note: String,
}

/// Shared gate. Sample / demo / unverified bars never enter lookalike search.
pub fn require_lookalike_bars(bars: &[Bar], operation: &str) -> Result<(), String> {
    require_research_bars(bars, operation)?;
    Bar::provenance(bars).require_verified(operation)?;
    Ok(())
}

pub fn feature_defs() -> Vec<FeatureDef> {
    FEATURE_NAMES
        .iter()
        .map(|name| FeatureDef {
            name: (*name).into(),
            lookback: match *name {
                "ret_1d" => 1,
                "ret_5d" => 5,
                "ret_10d" => 10,
                "rsi_14" => 14,
                "vol_21" | "ret_21d" | "range_21" => 21,
                "dist_sma20" | "volume_ratio" => 20,
                "dist_sma50" => 50,
                _ => FEATURE_LOOKBACK,
            },
        })
        .collect()
}

/// Parse `K=15 GAP=21 H=5,10,21 2024-01-15` (any order).
pub fn parse_query(spec: &str) -> Result<LookalikeQuery, String> {
    let mut q = LookalikeQuery::default();
    let tokens: Vec<&str> = spec.split_whitespace().filter(|t| !t.is_empty()).collect();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        let up = t.to_ascii_uppercase();
        if up == "GOTO" || up == "JUMP" || up == "GTO" {
            i += 1;
            continue;
        }
        if let Some(rest) = strip_prefix_ci(t, "K=") {
            q.k = parse_k(rest)?;
        } else if up == "K" {
            q.k = parse_k(next_token(&tokens, i, "K")?)?;
            i += 1;
        } else if let Some(rest) = strip_prefix_ci(t, "GAP=") {
            q.gap = parse_gap(rest)?;
        } else if up == "GAP" {
            q.gap = parse_gap(next_token(&tokens, i, "GAP")?)?;
            i += 1;
        } else if let Some(rest) = strip_prefix_ci(t, "H=") {
            let (raw, skip) = collect_horizon_tokens(rest, &tokens, i);
            q.horizons = parse_horizons(&raw)?;
            i += skip;
        } else if up == "H" || up == "HORIZONS" {
            let first = next_token(&tokens, i, "H")?;
            let (raw, skip) = collect_horizon_tokens(first, &tokens, i + 1);
            q.horizons = parse_horizons(&raw)?;
            i += 1 + skip;
        } else if journal::parse_chart_date(t).is_ok() {
            q.as_of = Some(journal::parse_chart_date(t)?);
        } else if t.chars().all(|c| c.is_ascii_digit()) {
            q.k = parse_k(t)?;
        } else {
            return Err(format!("Unrecognized lookalike option: {t}. Use K=20 GAP=21 H=5,10,21 and/or YYYY-MM-DD."));
        }
        i += 1;
    }
    if q.horizons.is_empty() {
        q.horizons = DEFAULT_HORIZONS.to_vec();
    }
    Ok(q)
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn next_token<'a>(tokens: &[&'a str], i: usize, name: &str) -> Result<&'a str, String> {
    tokens.get(i + 1).copied().ok_or_else(|| format!("{name} needs a value"))
}

fn parse_k(s: &str) -> Result<usize, String> {
    let k: usize = s.parse().map_err(|_| format!("K must be a positive integer, not {s:?}"))?;
    if k == 0 || k > 200 {
        return Err(format!("K must be 1…200, not {k}"));
    }
    Ok(k)
}

fn parse_gap(s: &str) -> Result<usize, String> {
    let g: usize = s.parse().map_err(|_| format!("GAP must be a session count, not {s:?}"))?;
    if g == 0 || g > 252 {
        return Err(format!("GAP must be 1…252 sessions, not {g}"));
    }
    Ok(g)
}

fn collect_horizon_tokens(first: &str, tokens: &[&str], from: usize) -> (String, usize) {
    let mut raw = first.to_string();
    let mut skip = 0;
    while from + 1 + skip < tokens.len() && looks_horizon_token(tokens[from + 1 + skip]) {
        raw.push(',');
        raw.push_str(tokens[from + 1 + skip]);
        skip += 1;
    }
    (raw, skip)
}

fn looks_horizon_token(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == ',' || c == ';' || c == '/')
}

pub fn parse_horizons(s: &str) -> Result<Vec<usize>, String> {
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let hs: Vec<usize> = compact
        .split(|c: char| c == ',' || c == ';' || c == '/')
        .filter_map(|t| t.parse().ok())
        .filter(|h| *h > 0 && *h <= 252)
        .collect();
    if hs.is_empty() {
        return Err(format!("Horizons need 1…252 trading days, not {s:?}"));
    }
    Ok(hs)
}

pub fn compute_report(
    symbol: &str,
    ticker: &str,
    bars: &[Bar],
    query: &LookalikeQuery,
    source: &str,
) -> Result<LookalikeReport, String> {
    require_lookalike_bars(bars, OPERATION)?;
    let provenance = Bar::provenance(bars);
    provenance.require_verified(OPERATION)?;

    let query_idx = resolve_query_index(bars, query.as_of.as_deref())?;
    if query_idx < FEATURE_LOOKBACK {
        return Err(format!(
            "{OPERATION} refused: {FEATURE_LOOKBACK} prior sessions are required for features (query is bar {}).",
            query_idx + 1
        ));
    }

    let features = feature_matrix(bars)?;
    if !features[query_idx].iter().all(|v| v.is_finite()) {
        return Err(format!(
            "{OPERATION} refused: query date {} does not have a complete backward feature window.",
            bar_date(bars[query_idx].ts)
        ));
    }

    let exclusion = query.gap.max(1);
    let eligible: Vec<usize> = (FEATURE_LOOKBACK..query_idx)
        .filter(|&i| query_idx - i >= exclusion)
        .filter(|&i| features[i].iter().all(|v| v.is_finite()))
        .collect();
    if eligible.is_empty() {
        return Err(format!(
            "{OPERATION} refused: no past sessions with complete features at least {exclusion} days before {}.",
            bar_date(bars[query_idx].ts)
        ));
    }

    let (means, stds) = standardize_stats(&features, &eligible, query_idx);
    let query_z = zscore(&features[query_idx], &means, &stds);
    let mut ranked: Vec<(usize, f64)> =
        eligible.iter().map(|&i| (i, euclidean(&query_z, &zscore(&features[i], &means, &stds)))).collect();
    ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));

    let k = query.k.max(1);
    let mut selected: Vec<(usize, f64)> = Vec::new();
    let mut discarded_overlap = 0usize;
    for (idx, dist) in ranked {
        if selected.iter().any(|(j, _)| idx.abs_diff(*j) < query.gap) {
            discarded_overlap += 1;
            continue;
        }
        selected.push((idx, dist));
        if selected.len() >= k {
            break;
        }
    }

    let neighbors: Vec<Neighbor> = selected
        .iter()
        .map(|&(i, distance)| Neighbor {
            date: bar_date(bars[i].ts),
            ts: bars[i].ts,
            index: i,
            distance,
            features: features[i].clone(),
            outcomes: query.horizons.iter().map(|&h| horizon_outcome(bars, i, h)).collect(),
        })
        .collect();

    let by_horizon: Vec<HorizonSummary> =
        query.horizons.iter().map(|&h| summarize_horizon(&neighbors, h, query.gap)).collect();

    let overlap_note = format!(
        "Requested K={k}. Selected N={} after a {}-session gap ({} nearer neighbors discarded as overlapping). \
         The {exclusion} sessions immediately before the query are excluded so a near-duplicate of “now” is not a neighbor. \
         Per-horizon completed / incomplete counts are listed before outcome frequencies. \
         Incomplete horizons are labelled and never filled in.",
        neighbors.len(),
        query.gap,
        discarded_overlap
    );

    Ok(LookalikeReport {
        symbol: symbol.into(),
        ticker: ticker.into(),
        provenance,
        source: provenance.source_label(source).into(),
        as_of: bar_date(bars[query_idx].ts),
        start: bars.first().map(|b| bar_date(b.ts)).unwrap_or_default(),
        end: bars.last().map(|b| bar_date(b.ts)).unwrap_or_default(),
        bars: bars.len(),
        query: QueryContext {
            date: bar_date(bars[query_idx].ts),
            ts: bars[query_idx].ts,
            index: query_idx,
            features: features[query_idx].clone(),
            z: query_z,
        },
        feature_names: FEATURE_NAMES.iter().map(|s| (*s).into()).collect(),
        feature_note: "Backward-looking only: 1/5/10/21-day close returns, RSI(14), 21-day return vol, \
                       close vs SMA(20)/SMA(50), volume / SMA(20) volume, 21-day high–low range / close. \
                       Distance is Euclidean on features z-scored across eligible past sessions plus the query."
            .into(),
        distance: "standardized Euclidean".into(),
        requested_k: k,
        selected_n: neighbors.len(),
        discarded_overlap,
        eligible_n: eligible.len(),
        gap: query.gap,
        neighbors,
        by_horizon,
        overlap_note,
    })
}

fn resolve_query_index(bars: &[Bar], as_of: Option<&str>) -> Result<usize, String> {
    let Some(as_of) = as_of else {
        return Ok(bars.len() - 1);
    };
    let date = journal::parse_chart_date(as_of)?;
    bars.iter()
        .rposition(|b| bar_date(b.ts) <= date)
        .ok_or_else(|| format!("{OPERATION} refused: no archived session on or before {date}."))
}

pub fn date_to_ts(date: &str) -> Result<i64, String> {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return Err(format!("chart date must be YYYY-MM-DD, not {date:?}"));
    }
    let y: i32 = parts[0].parse().map_err(|_| format!("bad year in {date}"))?;
    let m: u32 = parts[1].parse().map_err(|_| format!("bad month in {date}"))?;
    let d: u32 = parts[2].parse().map_err(|_| format!("bad day in {date}"))?;
    let nd = chrono::NaiveDate::from_ymd_opt(y, m, d).ok_or_else(|| format!("invalid calendar date {date}"))?;
    Ok(nd.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp())
}

fn feature_matrix(bars: &[Bar]) -> Result<Vec<Vec<f64>>, String> {
    let n = bars.len();
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let highs: Vec<f64> = bars.iter().map(|b| b.high).collect();
    let lows: Vec<f64> = bars.iter().map(|b| b.low).collect();
    let volumes: Vec<f64> = bars.iter().map(|b| b.volume).collect();
    let rsi14 = rsi(&closes, 14);
    let sma20 = sma(&closes, 20);
    let sma50 = sma(&closes, 50);
    let vol_sma20 = sma(&volumes, 20);
    let daily = daily_returns(bars);
    let mut out = vec![vec![f64::NAN; FEATURE_NAMES.len()]; n];
    for i in FEATURE_LOOKBACK..n {
        let ret = |lag: usize| {
            if i >= lag && bars[i - lag].close > 0.0 { Some(bars[i].close / bars[i - lag].close - 1.0) } else { None }
        };
        let vol21 = {
            let w: Vec<f64> = daily[i + 1 - 21..=i].iter().copied().flatten().collect();
            if w.len() == 21 {
                let m = w.iter().sum::<f64>() / 21.0;
                let var = w.iter().map(|x| (x - m).powi(2)).sum::<f64>() / 21.0;
                Some(var.max(0.0).sqrt())
            } else {
                None
            }
        };
        let range21 = {
            let hi = highs[i + 1 - 21..=i].iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let lo = lows[i + 1 - 21..=i].iter().copied().fold(f64::INFINITY, f64::min);
            if bars[i].close > 0.0 && hi.is_finite() && lo.is_finite() { Some((hi - lo) / bars[i].close) } else { None }
        };
        let row = [
            ret(1),
            ret(5),
            ret(10),
            ret(21),
            rsi14[i].is_finite().then_some(rsi14[i]),
            vol21,
            (sma20[i].is_finite() && sma20[i] != 0.0).then(|| bars[i].close / sma20[i] - 1.0),
            (sma50[i].is_finite() && sma50[i] != 0.0).then(|| bars[i].close / sma50[i] - 1.0),
            (vol_sma20[i].is_finite() && vol_sma20[i] > 0.0).then(|| bars[i].volume / vol_sma20[i]),
            range21,
        ];
        if row.iter().all(|v| v.is_some_and(|x| x.is_finite())) {
            out[i] = row.into_iter().map(|v| v.unwrap()).collect();
        }
    }
    Ok(out)
}

fn daily_returns(bars: &[Bar]) -> Vec<Option<f64>> {
    (0..bars.len())
        .map(|i| if i == 0 || bars[i - 1].close <= 0.0 { None } else { Some(bars[i].close / bars[i - 1].close - 1.0) })
        .collect()
}

fn standardize_stats(features: &[Vec<f64>], eligible: &[usize], query_idx: usize) -> (Vec<f64>, Vec<f64>) {
    let dim = FEATURE_NAMES.len();
    let mut means = vec![0.0; dim];
    let mut stds = vec![0.0; dim];
    let mut idxs = eligible.to_vec();
    idxs.push(query_idx);
    let n = idxs.len() as f64;
    for j in 0..dim {
        let sum: f64 = idxs.iter().map(|&i| features[i][j]).sum();
        means[j] = sum / n;
        let var: f64 = idxs.iter().map(|&i| (features[i][j] - means[j]).powi(2)).sum::<f64>() / n;
        stds[j] = var.max(0.0).sqrt();
    }
    (means, stds)
}

fn zscore(row: &[f64], means: &[f64], stds: &[f64]) -> Vec<f64> {
    row.iter().zip(means).zip(stds).map(|((x, m), s)| if *s > 1e-12 { (x - m) / s } else { 0.0 }).collect()
}

fn euclidean(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f64>().sqrt()
}

fn horizon_outcome(bars: &[Bar], i: usize, horizon: usize) -> HorizonOutcome {
    let end = i + horizon;
    if end >= bars.len() {
        let have = bars.len().saturating_sub(i + 1);
        return HorizonOutcome {
            horizon,
            completed: false,
            ret: None,
            reason: Some(format!(
                "incomplete: only {have} session{} after {} (need {horizon})",
                if have == 1 { "" } else { "s" },
                bar_date(bars[i].ts)
            )),
        };
    }
    if bars[i].close <= 0.0 || !bars[end].close.is_finite() {
        return HorizonOutcome {
            horizon,
            completed: false,
            ret: None,
            reason: Some("incomplete: invalid close on the forward bar".into()),
        };
    }
    HorizonOutcome { horizon, completed: true, ret: Some(bars[end].close / bars[i].close - 1.0), reason: None }
}

fn summarize_horizon(neighbors: &[Neighbor], horizon: usize, gap: usize) -> HorizonSummary {
    let requested_n = neighbors.len();
    let mut completed = Vec::new();
    let mut incomplete_n = 0usize;
    for n in neighbors {
        match n.outcomes.iter().find(|o| o.horizon == horizon) {
            Some(o) if o.completed => {
                if let Some(r) = o.ret.filter(|v| v.is_finite()) {
                    completed.push(r);
                } else {
                    incomplete_n += 1;
                }
            }
            _ => incomplete_n += 1,
        }
    }
    let completed_n = completed.len();
    let span = gap.min(horizon).max(1);
    let effective_n = completed_n as f64 * span as f64 / horizon.max(1) as f64;
    let overlap_note = if gap >= horizon {
        format!("gap {gap} ≥ H {horizon}: completed outcomes do not share days. Effective N = completed N.")
    } else {
        format!(
            "gap {gap} < H {horizon}: H-day outcomes can share days. Effective N = completed × gap / H = {effective_n:.2}."
        )
    };
    completed.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    HorizonSummary {
        horizon,
        requested_n,
        completed_n,
        incomplete_n,
        effective_n,
        overlap_note,
        mean: mean_of(&completed),
        median: percentile(&completed, 0.50),
        std: std_of(&completed),
        win_rate: if completed.is_empty() {
            None
        } else {
            Some(completed.iter().filter(|r| **r > 0.0).count() as f64 / completed_n as f64)
        },
        p25: percentile(&completed, 0.25),
        p75: percentile(&completed, 0.75),
        min: completed.first().copied(),
        max: completed.last().copied(),
    }
}

fn mean_of(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() { None } else { Some(xs.iter().sum::<f64>() / xs.len() as f64) }
}

fn std_of(xs: &[f64]) -> Option<f64> {
    if xs.len() < 2 {
        return None;
    }
    let m = mean_of(xs)?;
    let var = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (xs.len() as f64 - 1.0);
    Some(var.max(0.0).sqrt())
}

fn percentile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = q.clamp(0.0, 1.0) * (sorted.len() as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        Some(sorted[lo])
    } else {
        let t = rank - lo as f64;
        Some(sorted[lo] * (1.0 - t) + sorted[hi] * t)
    }
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

    fn uptrend(n: usize) -> Vec<Bar> {
        series(n, |i| 100.0 + i as f64 * 0.2)
    }

    #[test]
    fn sample_and_unverified_bars_never_enter_lookalikes() {
        let sample = vec![Bar::new(day(0), 1.0, 1.0, 1.0, 1.0, 1.0).with_provenance(Provenance::Sample)];
        let unverified = vec![Bar::new(day(0), 1.0, 1.0, 1.0, 1.0, 1.0)];
        let mixed = vec![real(day(0), 10.0), sample[0]];
        for (bars, needle) in [(&sample, "SAMPLE"), (&unverified, "UNVERIFIED"), (&mixed, "SAMPLE")] {
            let err = compute_report("AAPL US Equity", "AAPL", bars, &LookalikeQuery::default(), "test").unwrap_err();
            assert!(err.contains(needle), "{err}");
            assert!(err.contains("Lookalikes") || err.contains("refused"), "{err}");
        }
        assert!(require_lookalike_bars(&[], "Lookalikes").unwrap_err().contains("no daily history"));
    }

    #[test]
    fn incomplete_forward_horizons_are_labelled_not_invented() {
        // Query is the last bar: every horizon is unfinished.
        let bars = uptrend(120);
        let q = LookalikeQuery { k: 5, horizons: vec![5, 21], as_of: None, gap: 5 };
        let report = compute_report("X", "X", &bars, &q, "unit test").unwrap();
        assert_eq!(report.as_of, bar_date(bars[119].ts));
        assert_eq!(report.provenance, Provenance::Real);
        assert_eq!(report.source, "unit test");
        let h21 = report.by_horizon.iter().find(|h| h.horizon == 21).unwrap();
        assert_eq!(h21.completed_n + h21.incomplete_n, report.selected_n);
        assert!(h21.incomplete_n > 0, "neighbors near the end must stay incomplete");
        for n in &report.neighbors {
            if n.index + 21 >= bars.len() {
                let o = n.outcomes.iter().find(|o| o.horizon == 21).unwrap();
                assert!(!o.completed);
                assert!(o.ret.is_none());
                assert!(o.reason.as_ref().unwrap().contains("incomplete"));
            }
        }
        assert!(report.overlap_note.contains("completed"));
        assert!(h21.overlap_note.contains("completed"));
    }

    #[test]
    fn overlapping_neighbors_are_reduced_and_effective_n_labelled() {
        let bars = uptrend(160);
        let as_of = bar_date(bars[140].ts);
        let tight = LookalikeQuery { k: 8, horizons: vec![10], as_of: Some(as_of.clone()), gap: 1 };
        let spaced = LookalikeQuery { k: 8, horizons: vec![10], as_of: Some(as_of), gap: 10 };
        let a = compute_report("X", "X", &bars, &tight, "t").unwrap();
        let b = compute_report("X", "X", &bars, &spaced, "t").unwrap();
        assert!(a.discarded_overlap < b.discarded_overlap || a.selected_n >= b.selected_n);
        for (i, n) in a.neighbors.iter().enumerate() {
            assert!(a.neighbors.iter().enumerate().all(|(j, o)| i == j || n.index.abs_diff(o.index) >= 1));
        }
        for (i, n) in b.neighbors.iter().enumerate() {
            assert!(
                b.neighbors.iter().enumerate().all(|(j, o)| i == j || n.index.abs_diff(o.index) >= 10),
                "{} vs another neighbor closer than the gap",
                n.index
            );
        }
        let h = &b.by_horizon[0];
        assert_eq!(h.requested_n, b.selected_n);
        assert_eq!(h.completed_n + h.incomplete_n, b.selected_n);
        assert!((h.effective_n - h.completed_n as f64).abs() < 1e-9, "gap >= H → effective N = completed");
        let tight_h = &a.by_horizon[0];
        assert!(tight_h.overlap_note.contains("gap 1 < H 10"));
        assert!(tight_h.effective_n < tight_h.completed_n as f64 + 1e-9);
    }

    #[test]
    fn query_and_future_bars_are_never_neighbors() {
        let bars = uptrend(140);
        let as_of = bar_date(bars[100].ts);
        let report = compute_report(
            "X",
            "X",
            &bars,
            &LookalikeQuery { k: 20, horizons: vec![5], as_of: Some(as_of), gap: 5 },
            "t",
        )
        .unwrap();
        assert_eq!(report.query.index, 100);
        assert!(report.neighbors.iter().all(|n| n.index < 100));
        assert!(report.neighbors.iter().all(|n| n.ts < report.query.ts));
        assert!(!report.neighbors.iter().any(|n| n.index == 100));
    }

    #[test]
    fn per_horizon_counts_precede_frequencies() {
        let bars = uptrend(180);
        let report = compute_report(
            "X",
            "X",
            &bars,
            &LookalikeQuery { k: 6, horizons: vec![5, 10, 21], as_of: Some(bar_date(bars[150].ts)), gap: 8 },
            "t",
        )
        .unwrap();
        assert_eq!(report.by_horizon.len(), 3);
        for h in &report.by_horizon {
            assert_eq!(h.completed_n + h.incomplete_n, h.requested_n);
            if h.completed_n > 0 {
                assert!(h.mean.is_some());
                assert!(h.win_rate.is_some());
            }
        }
        // Earlier horizon completes more often than a longer unfinished one near the end.
        assert!(report.by_horizon[0].completed_n >= report.by_horizon[2].completed_n);
    }

    #[test]
    fn repeated_pattern_is_a_near_neighbor() {
        // Quiet grind, then the same drop-and-bounce twice. Query sits on the later bounce.
        let mut px = 100.0;
        let bars = series(220, |i| {
            if (80..=95).contains(&i) || (180..=195).contains(&i) {
                px *= if i <= 88 || (180..=188).contains(&i) { 0.97 } else { 1.025 };
            } else {
                px *= 1.001;
            }
            px
        });
        let report = compute_report(
            "X",
            "X",
            &bars,
            &LookalikeQuery { k: 5, horizons: vec![5], as_of: Some(bar_date(bars[195].ts)), gap: 10 },
            "t",
        )
        .unwrap();
        assert!(report.selected_n > 0);
        let nearest = &report.neighbors[0];
        assert!(
            (80..=100).contains(&nearest.index),
            "expected the earlier drop/bounce, got index {} date {}",
            nearest.index,
            nearest.date
        );
        assert!(nearest.distance.is_finite());
        assert!(report.distance.contains("Euclidean"));
        assert_eq!(report.feature_names.len(), 10);
    }

    #[test]
    fn parse_query_reads_k_gap_horizons_and_date() {
        let q = parse_query("2024-03-15 K=12 GAP=8 H=5,21").unwrap();
        assert_eq!(q.as_of.as_deref(), Some("2024-03-15"));
        assert_eq!(q.k, 12);
        assert_eq!(q.gap, 8);
        assert_eq!(q.horizons, vec![5, 21]);
        let q = parse_query("K 7 GOTO 2026-01-02").unwrap();
        assert_eq!(q.k, 7);
        assert_eq!(q.as_of.as_deref(), Some("2026-01-02"));
        let q = parse_query("H=5, 10, 21").unwrap();
        assert_eq!(q.horizons, vec![5, 10, 21]);
        assert!(parse_query("K=0").is_err());
        assert!(parse_query("nope").is_err());
    }

    #[test]
    fn as_of_before_the_series_is_refused() {
        let bars = uptrend(80);
        let err = compute_report(
            "X",
            "X",
            &bars,
            &LookalikeQuery { k: 3, horizons: vec![5], as_of: Some("1999-01-01".into()), gap: 5 },
            "t",
        )
        .unwrap_err();
        assert!(err.contains("no archived session"), "{err}");
    }
}
