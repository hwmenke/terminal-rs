//! SCAN calculation contract, shared by both archives and HTTP enrichment.
//!
//! RSI is Wilder's 14-change SMA seed followed by 1/14 smoothing (flat = 50).
//! Strength is the exact 21-bar close return, ranked within each as-of-date cohort.
//! Volume surge uses last volume / previous 20 mean; ADR is mean((H-L)/C)*100
//! over exactly 20 bars. Thresholds use unrounded values. Score = near-high +
//! surge + EP + 2 for Darvas breakout, or 1 for a box within 2% of its top.
//! W/M use calendar buckets from ALL daily bars, omitting incomplete edge buckets.
//! Conditions are close versus SMA20 and RSI14, never shortened lookbacks.
use super::*;
use chrono::Duration as Days;
use std::{
    cmp::Ordering,
    sync::{Mutex, OnceLock},
};
use tr_core::Timeframe;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Conditions {
    pub bars: usize,
    pub rsi: Option<f64>,
    pub above_sma20: Option<bool>,
    pub unavailable: Option<String>,
}
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Metrics {
    pub change_pct: f64,
    pub ret_21d_pct: f64,
    pub rs_rank: usize,
    pub rs_n: usize,
    pub volume: f64,
    pub avg_volume20: f64,
    pub volume_ratio: Option<f64>,
    pub vol_ratio_5_20: Option<f64>,
    pub adr_pct: f64,
    pub dist_20d_high_pct: f64,
    pub gap_pct: f64,
    pub score: f64,
    pub daily: Conditions,
    pub weekly: Conditions,
    pub monthly: Conditions,
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Skipped {
    pub symbol: String,
    pub reason: String,
}

pub fn rsi(bars: &[Bar], period: usize) -> Option<f64> {
    if period == 0 || bars.len() <= period {
        return None;
    }
    let changes: Vec<_> = bars.windows(2).map(|b| b[1].close - b[0].close).collect();
    let mut gain = changes[..period].iter().map(|x| x.max(0.0)).sum::<f64>() / period as f64;
    let mut loss = changes[..period].iter().map(|x| (-x).max(0.0)).sum::<f64>() / period as f64;
    for delta in &changes[period..] {
        gain = (gain * (period - 1) as f64 + delta.max(0.0)) / period as f64;
        loss = (loss * (period - 1) as f64 + (-delta).max(0.0)) / period as f64;
    }
    Some(if gain == 0.0 && loss == 0.0 {
        50.0
    } else if loss == 0.0 {
        100.0
    } else {
        100.0 - 100.0 / (1.0 + gain / loss)
    })
}

/// Only full calendar periods supported by the archived date span. No padding,
/// synthetic months, or fixed 30-day approximations; February and years stay distinct.
pub fn completed_periods(bars: &[Bar], tf: Timeframe) -> Vec<Bar> {
    if bars.is_empty() {
        return vec![];
    }
    if tf == Timeframe::D1 {
        return bars.to_vec();
    }
    let first = chrono::DateTime::from_timestamp(bars[0].ts, 0).unwrap().date_naive();
    let last = chrono::DateTime::from_timestamp(bars.last().unwrap().ts, 0).unwrap().date_naive();
    tf.resample(bars)
        .into_iter()
        .filter(|b| {
            let start = chrono::DateTime::from_timestamp(b.ts, 0).unwrap().date_naive();
            let end = match tf {
                Timeframe::W1 => start + Days::days(6),
                Timeframe::MN1 => {
                    let Some(next) = start.checked_add_months(chrono::Months::new(1)) else {
                        return false;
                    };
                    next - Days::days(1)
                }
                _ => return false,
            };
            let mut first_session = start;
            while !is_session(first_session) {
                first_session += Days::days(1);
            }
            let mut last_session = end;
            while !is_session(last_session) {
                last_session -= Days::days(1);
            }
            first <= first_session && last >= last_session
        })
        .collect()
}
fn conditions(bars: &[Bar], label: &str) -> Conditions {
    if bars.len() < 20 {
        return Conditions {
            bars: bars.len(),
            unavailable: Some(format!("insufficient {label} history: need 20 bars, have {}", bars.len())),
            ..Default::default()
        };
    }
    let sma = bars[bars.len() - 20..].iter().map(|b| b.close / 20.0).sum::<f64>();
    Conditions {
        bars: bars.len(),
        rsi: rsi(bars, 14),
        above_sma20: Some(bars.last().unwrap().close >= sma),
        unavailable: None,
    }
}
fn validate(bars: &[Bar]) -> std::result::Result<(), String> {
    if bars.is_empty() {
        return Err("missing daily history".into());
    }
    if bars.iter().any(|b| {
        chrono::DateTime::from_timestamp(b.ts, 0).is_none()
            || ![b.open, b.high, b.low, b.close, b.volume].iter().all(|v| v.is_finite())
            || b.low <= 0.0
            || b.open <= 0.0
            || b.close <= 0.0
            || b.volume < 0.0
            || b.high < b.open.max(b.close).max(b.low)
            || b.low > b.open.min(b.close)
    }) {
        return Err("invalid archived OHLCV or date".into());
    }
    if bars.windows(2).any(|b| b[0].ts >= b[1].ts) {
        return Err("daily bars must have unique ascending dates".into());
    }
    if bars.len() < 30 {
        return Err(format!("insufficient daily history: need 30 bars, have {}", bars.len()));
    }
    Ok(())
}
pub fn calculate(b: &[Bar]) -> std::result::Result<Metrics, String> {
    validate(b)?;
    let n = b.len();
    let last = b[n - 1];
    let avg = b[n - 21..n - 1].iter().map(|b| b.volume / 20.0).sum::<f64>();
    let current_avg = b[n - 20..].iter().map(|b| b.volume / 20.0).sum::<f64>();
    let ratio = (avg > 0.0).then(|| last.volume / avg);
    let dist = (last.close / b[n - 20..].iter().map(|b| b.high).fold(0.0, f64::max) - 1.0) * 100.0;
    let gap = (last.open / b[n - 2].close - 1.0) * 100.0;
    let near = dist >= -5.0;
    let surge = ratio.is_some_and(|v| v >= 1.5);
    let ep = gap >= 4.0 && surge;
    let box_ = darvas(b);
    let score = near as u8
        + surge as u8
        + ep as u8
        + match box_ {
            Some((_, _, "DARVAS_BREAKOUT")) => 2,
            Some((_, _, "DARVAS_BOX")) if dist >= -2.0 => 1,
            _ => 0,
        };
    let metrics = Metrics {
        change_pct: (last.close / b[n - 2].close - 1.0) * 100.0,
        ret_21d_pct: (last.close / b[n - 22].close - 1.0) * 100.0,
        volume: last.volume,
        avg_volume20: avg,
        volume_ratio: ratio,
        vol_ratio_5_20: (current_avg > 0.0)
            .then(|| b[n - 5..].iter().map(|b| b.volume).sum::<f64>() / 5.0 / current_avg),
        adr_pct: b[n - 20..].iter().map(|b| (b.high - b.low) / b.close * 100.0).sum::<f64>() / 20.0,
        dist_20d_high_pct: dist,
        gap_pct: gap,
        score: score as f64,
        daily: conditions(b, "daily"),
        weekly: conditions(&completed_periods(b, Timeframe::W1), "completed weekly"),
        monthly: conditions(&completed_periods(b, Timeframe::MN1), "completed monthly"),
        ..Default::default()
    };
    if [
        Some(metrics.change_pct),
        Some(metrics.ret_21d_pct),
        Some(metrics.gap_pct),
        Some(metrics.adr_pct),
        Some(metrics.dist_20d_high_pct),
        Some(metrics.avg_volume20),
        metrics.volume_ratio,
        metrics.vol_ratio_5_20,
        metrics.daily.rsi,
        metrics.weekly.rsi,
        metrics.monthly.rsi,
    ]
    .into_iter()
    .flatten()
    .any(|v| !v.is_finite())
    {
        return Err("non-finite calculation from archived bars".into());
    }
    Ok(metrics)
}
#[derive(Default)]
struct CalculationCache {
    entries: Vec<(Vec<Bar>, std::result::Result<Metrics, String>)>,
}
impl CalculationCache {
    fn get(&mut self, bars: &[Bar]) -> std::result::Result<Metrics, String> {
        if let Some((_, metrics)) = self.entries.iter().find(|(b, _)| b == bars) {
            return metrics.clone();
        }
        let metrics = calculate(bars);
        if self.entries.len() == 64 {
            let _ = self.entries.remove(0);
        }
        self.entries.push((bars.to_vec(), metrics.clone()));
        metrics
    }
}
pub(super) fn cached_calculate(bars: &[Bar]) -> std::result::Result<Metrics, String> {
    static CACHE: OnceLock<Mutex<CalculationCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(CalculationCache::default())).lock().unwrap().get(bars)
}
/// Whether `name` is active on verified daily bars. Sample / unverified history is refused.
pub fn setup_active(name: &str, bars: &[Bar]) -> std::result::Result<bool, String> {
    if !super::SETUPS.contains(&name) {
        return Err(format!("unknown setup {name}"));
    }
    Bar::require_verified(bars, "price/setup alert")?;
    let m = cached_calculate(bars)?;
    Ok(matches_setup(name, bars, &m))
}

fn matches_setup(name: &str, b: &[Bar], m: &Metrics) -> bool {
    let near = m.dist_20d_high_pct >= -5.0;
    let surge = m.volume_ratio.is_some_and(|v| v >= 1.5);
    match name {
        "NEAR_HIGH" => near,
        "VOL_SURGE" => surge,
        "EP" => m.gap_pct >= 4.0 && surge,
        "BREAKOUT_QUEUE" => near || surge,
        "RSI_OB" => m.daily.rsi.is_some_and(|r| r >= 70.0),
        "RSI_OS" => m.daily.rsi.is_some_and(|r| r <= 30.0),
        _ => darvas(b).is_some_and(|(_, _, state)| state == name),
    }
}

/// Bounded, exact-content cache. Full bar/provenance equality catches corrections
/// with unchanged last dates/counts, deletions and archive switches. Clock-dependent
/// staleness is deliberately evaluated by the view after this cache.
#[derive(Default)]
pub struct ScanCache {
    entries: Vec<(String, Provenance, Vec<(String, Vec<Bar>)>, Scan)>,
}
impl ScanCache {
    pub fn scan(
        &mut self,
        name: &str,
        histories: Vec<(String, Vec<Bar>)>,
        provenance: Provenance,
    ) -> Result<(Scan, bool)> {
        verify(provenance, "Whats-News scan")?;
        for (_, bars) in &histories {
            if !bars.is_empty() {
                verify(Bar::provenance(bars), "Whats-News scan")?;
            }
        }
        if let Some((_, _, _, scan)) =
            self.entries.iter().find(|(n, p, h, _)| n == name && *p == provenance && *h == histories)
        {
            return Ok((scan.clone(), true));
        }
        if !SETUPS.contains(&name) {
            bail!("Unknown scan {name}");
        }
        let mut scan = Scan { provenance, scanned: 0, results: vec![], skipped: vec![] };
        let mut candidates = vec![];
        for (symbol, b) in &histories {
            let m = match cached_calculate(b) {
                Ok(m) => m,
                Err(reason) => {
                    scan.skipped.push(Skipped { symbol: symbol.clone(), reason });
                    continue;
                }
            };
            scan.scanned += 1;
            let box_ = darvas(b);
            candidates.push((
                matches_setup(name, b, &m),
                ScanRow {
                    provenance,
                    symbol: symbol.clone(),
                    price: Some(b.last().unwrap().close),
                    setup_score: Some(m.score),
                    darvas_top: box_.map(|b| b.0),
                    darvas_bottom: box_.map(|b| b.1),
                    last_date: Some(date(b.last().unwrap().ts)),
                    metrics: Some(m),
                    ..Default::default()
                },
            ));
        }
        // Rank the eligible archive universe before setup/filter selection. Ties share
        // competition rank; different dates never compete against one another.
        let mut cohorts = std::collections::BTreeMap::<Option<String>, Vec<usize>>::new();
        for (i, (_, row)) in candidates.iter().enumerate() {
            cohorts.entry(row.last_date.clone()).or_default().push(i);
        }
        for indices in cohorts.values_mut() {
            indices.sort_by(|a, b| {
                candidates[*b]
                    .1
                    .metrics
                    .as_ref()
                    .unwrap()
                    .ret_21d_pct
                    .total_cmp(&candidates[*a].1.metrics.as_ref().unwrap().ret_21d_pct)
            });
            let mut rank = 1;
            let mut previous = None;
            let count = indices.len();
            for (place, i) in indices.iter().enumerate() {
                let m = candidates[*i].1.metrics.as_mut().unwrap();
                if previous.is_some_and(|ret| ret != m.ret_21d_pct) {
                    rank = place + 1;
                }
                previous = Some(m.ret_21d_pct);
                m.rs_rank = rank;
                m.rs_n = count;
            }
        }
        scan.results = candidates.into_iter().filter(|(matched, _)| *matched).map(|(_, r)| r).collect();
        if self.entries.len() == 2 {
            self.entries.remove(0);
        }
        self.entries.push((name.into(), provenance, histories, scan.clone()));
        Ok((scan, false))
    }
}
pub(super) fn scan_cached(name: &str, histories: Vec<(String, Vec<Bar>)>, provenance: Provenance) -> Result<Scan> {
    static CACHE: OnceLock<Mutex<ScanCache>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(ScanCache::default()))
        .lock()
        .unwrap()
        .scan(name, histories, provenance)
        .map(|(s, _)| s)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Condition {
    #[default]
    Any,
    AboveSma20,
    BelowSma20,
    Overbought,
    Oversold,
}
impl Condition {
    pub const ALL: [Self; 5] = [Self::Any, Self::AboveSma20, Self::BelowSma20, Self::Overbought, Self::Oversold];
    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "Any",
            Self::AboveSma20 => "Close ≥ SMA20",
            Self::BelowSma20 => "Close < SMA20",
            Self::Overbought => "RSI ≥ 70",
            Self::Oversold => "RSI ≤ 30",
        }
    }
    fn accepts(self, c: &Conditions) -> bool {
        match self {
            Self::Any => true,
            Self::AboveSma20 => c.above_sma20 == Some(true),
            Self::BelowSma20 => c.above_sma20 == Some(false),
            Self::Overbought => c.rsi.is_some_and(|r| r >= 70.0),
            Self::Oversold => c.rsi.is_some_and(|r| r <= 30.0),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    Symbol,
    #[default]
    Score,
    Strength,
    Rsi,
    Volume,
    VolumeRatio,
    Range,
}
impl Sort {
    pub const ALL: [Self; 7] =
        [Self::Symbol, Self::Score, Self::Strength, Self::Rsi, Self::Volume, Self::VolumeRatio, Self::Range];
    pub fn label(self) -> &'static str {
        match self {
            Self::Symbol => "Symbol",
            Self::Score => "Rust score",
            Self::Strength => "21-bar return",
            Self::Rsi => "Daily RSI14",
            Self::Volume => "Volume",
            Self::VolumeRatio => "Volume / prior 20",
            Self::Range => "ADR20 %",
        }
    }
    fn value(self, r: &ScanRow) -> Option<f64> {
        let m = r.metrics.as_ref()?;
        match self {
            Self::Symbol => None,
            Self::Score => Some(m.score),
            Self::Strength => Some(m.ret_21d_pct),
            Self::Rsi => m.daily.rsi,
            Self::Volume => Some(m.volume),
            Self::VolumeRatio => m.volume_ratio,
            Self::Range => Some(m.adr_pct),
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct ScanFilter {
    pub include_stale: bool,
    pub min_score: Option<f64>,
    pub min_strength: Option<f64>,
    pub min_rsi: Option<f64>,
    pub max_rsi: Option<f64>,
    pub min_volume: Option<f64>,
    pub min_volume_ratio: Option<f64>,
    pub min_range: Option<f64>,
    pub max_range: Option<f64>,
    pub daily: Condition,
    pub weekly: Condition,
    pub monthly: Condition,
    pub sort: Sort,
    pub descending: bool,
}
impl Default for ScanFilter {
    fn default() -> Self {
        Self {
            include_stale: false,
            min_score: None,
            min_strength: None,
            min_rsi: None,
            max_rsi: None,
            min_volume: None,
            min_volume_ratio: None,
            min_range: None,
            max_range: None,
            daily: Condition::Any,
            weekly: Condition::Any,
            monthly: Condition::Any,
            sort: Sort::Score,
            descending: true,
        }
    }
}
impl ScanFilter {
    pub fn reason(&self, r: &ScanRow, expected: NaiveDate) -> Option<String> {
        let date = r.last_date.as_deref().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
        if !self.include_stale {
            match date {
                None => return Some("missing last daily bar date".into()),
                Some(d) if d < expected => return Some(format!("stale: last bar {d}, expected {expected}")),
                _ => {}
            }
        }
        let checks = [
            (self.min_score, r.metrics.as_ref().map(|m| m.score), false, "score"),
            (self.min_strength, r.metrics.as_ref().map(|m| m.ret_21d_pct), false, "21-bar return"),
            (self.min_rsi, r.metrics.as_ref().and_then(|m| m.daily.rsi), false, "RSI14"),
            (self.max_rsi, r.metrics.as_ref().and_then(|m| m.daily.rsi), true, "RSI14"),
            (self.min_volume, r.metrics.as_ref().map(|m| m.volume), false, "volume"),
            (self.min_volume_ratio, r.metrics.as_ref().and_then(|m| m.volume_ratio), false, "volume ratio"),
            (self.min_range, r.metrics.as_ref().map(|m| m.adr_pct), false, "ADR20"),
            (self.max_range, r.metrics.as_ref().map(|m| m.adr_pct), true, "ADR20"),
        ];
        for (limit, value, max, label) in checks {
            if let Some(limit) = limit {
                match value.filter(|v| v.is_finite()) {
                    None => return Some(format!("{label} unavailable from archived history")),
                    Some(v) if (max && v > limit) || (!max && v < limit) => {
                        return Some(format!("{label} outside filter"));
                    }
                    _ => {}
                }
            }
        }
        for (cond, label) in [(self.daily, "daily"), (self.weekly, "weekly"), (self.monthly, "monthly")] {
            if cond == Condition::Any {
                continue;
            }
            let c = r.metrics.as_ref().map(|m| match label {
                "daily" => &m.daily,
                "weekly" => &m.weekly,
                _ => &m.monthly,
            });
            match c {
                None => return Some(format!("{label} conditions unavailable from archived history")),
                Some(c) if c.unavailable.is_some() => return c.unavailable.clone(),
                Some(c) if !cond.accepts(c) => return Some(format!("{label}: {} not met", cond.label())),
                _ => {}
            }
        }
        None
    }
    pub fn rows<'a>(&self, scan: &'a Scan, query: &str, expected: NaiveDate) -> Vec<&'a ScanRow> {
        let query = query.trim().to_ascii_uppercase();
        let mut rows: Vec<_> = scan
            .results
            .iter()
            .filter(|r| r.symbol.to_ascii_uppercase().contains(&query) && self.reason(r, expected).is_none())
            .collect();
        rows.sort_by(|a, b| {
            let cmp = if self.sort == Sort::Symbol {
                if self.descending { b.symbol.cmp(&a.symbol) } else { a.symbol.cmp(&b.symbol) }
            } else {
                match (self.sort.value(a).filter(|v| v.is_finite()), self.sort.value(b).filter(|v| v.is_finite())) {
                    (Some(a), Some(b)) => {
                        if self.descending {
                            b.total_cmp(&a)
                        } else {
                            a.total_cmp(&b)
                        }
                    }
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    _ => Ordering::Equal,
                }
            };
            cmp.then_with(|| a.symbol.cmp(&b.symbol))
        });
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }
    // Artificial prices remain isolated in unit tests.
    fn bar(day: NaiveDate, close: f64) -> Bar {
        Bar::new(day.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(), close, close + 1.0, close - 1.0, close, 100.0)
            .with_provenance(Provenance::Real)
    }
    fn bars(n: usize) -> Vec<Bar> {
        (0..n).map(|i| bar(d("2024-01-02") + Days::days(i as i64), 100.0)).collect()
    }
    fn row(symbol: &str, value: f64) -> ScanRow {
        ScanRow {
            symbol: symbol.into(),
            provenance: Provenance::Real,
            last_date: Some("2026-10-21".into()),
            metrics: Some(Metrics {
                score: value,
                ret_21d_pct: value,
                volume: value,
                volume_ratio: Some(value),
                adr_pct: value,
                daily: Conditions { rsi: Some(value), above_sma20: Some(true), bars: 30, unavailable: None },
                weekly: Conditions { rsi: Some(75.0), above_sma20: Some(true), bars: 30, unavailable: None },
                monthly: Conditions { rsi: Some(20.0), above_sma20: Some(false), bars: 30, unavailable: None },
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-8, "{a} != {b}");
    }
    #[test]
    fn wilder_rsi_seed_smoothing_and_extremes() {
        let prices = [
            44.34, 44.09, 44.15, 43.61, 44.33, 44.83, 45.10, 45.42, 45.84, 46.08, 45.89, 46.03, 45.61, 46.28, 46.28,
            46.0,
        ];
        let b: Vec<_> =
            prices.iter().enumerate().map(|(i, c)| bar(d("2026-01-01") + Days::days(i as i64), *c)).collect();
        close(rsi(&b[..15], 14).unwrap(), 70.46413502109705);
        close(rsi(&b, 14).unwrap(), 66.24961855355505);
        assert_eq!(rsi(&b[..14], 14), None);
        assert_eq!(rsi(&b, 0), None);
        assert_eq!(rsi(&bars(30), 14), Some(50.0));
        let rising: Vec<_> = (0..30).map(|i| bar(d("2024-01-02") + Days::days(i), 100.0 + i as f64)).collect();
        assert_eq!(rsi(&rising, 14), Some(100.0));
        let falling: Vec<_> = (0..30).map(|i| bar(d("2024-01-02") + Days::days(i), 100.0 - i as f64)).collect();
        assert_eq!(rsi(&falling, 14), Some(0.0));
    }
    #[test]
    fn exact_returns_volume_range_gap_distance_score_and_conditions() {
        let mut b = bars(60);
        for b in &mut b {
            b.high = 110.0;
            b.low = 90.0;
        }
        let last = b.last_mut().unwrap();
        last.open = 105.0;
        last.close = 106.0;
        last.volume = 200.0;
        let m = calculate(&b).unwrap();
        close(m.change_pct, 6.0);
        close(m.ret_21d_pct, 6.0);
        close(m.gap_pct, 5.0);
        close(m.volume, 200.0);
        close(m.avg_volume20, 100.0);
        close(m.volume_ratio.unwrap(), 2.0);
        close(m.vol_ratio_5_20.unwrap(), 120.0 / 105.0);
        close(m.adr_pct, (19.0 * 20.0 + 20.0 / 106.0 * 100.0) / 20.0);
        close(m.dist_20d_high_pct, (106.0 / 110.0 - 1.0) * 100.0);
        assert_eq!(m.score, 3.0);
        assert_eq!(m.daily.above_sma20, Some(true));
        assert_eq!(m.daily.rsi, Some(100.0));
        assert!(m.monthly.unavailable.as_deref().unwrap().contains("need 20"));
        assert!(matches_setup("NEAR_HIGH", &b, &m));
        assert!(matches_setup("VOL_SURGE", &b, &m));
        assert!(matches_setup("EP", &b, &m));
        assert!(matches_setup("BREAKOUT_QUEUE", &b, &m));
        assert!(matches_setup("RSI_OB", &b, &m));
        assert!(!matches_setup("RSI_OS", &b, &m));
        assert!(matches_setup("DARVAS_BOX", &b, &m));
        for b in &mut b {
            b.volume = 0.0;
        }
        let m = calculate(&b).unwrap();
        assert_eq!(m.volume_ratio, None);
        assert_eq!(m.vol_ratio_5_20, None);
        assert_eq!(m.avg_volume20, 0.0);
        assert!(!matches_setup("VOL_SURGE", &b, &m));
        assert!(!matches_setup("EP", &b, &m));
    }
    #[test]
    fn score_box_bonus_breakout_failure_and_thresholds_do_not_round() {
        let mut b = bars(30);
        for b in &mut b {
            b.high = 101.0;
            b.low = 90.0;
        }
        let m = calculate(&b).unwrap();
        assert_eq!(m.score, 2.0); // near high + box bonus
        b[29].close = 105.0;
        b[29].open = 105.0;
        b[29].high = 106.0;
        b[29].volume = 200.0;
        let m = calculate(&b).unwrap();
        assert_eq!(m.score, 5.0);
        assert!(matches_setup("DARVAS_BREAKOUT", &b, &m));
        b[29].close = 89.0;
        b[29].open = 89.0;
        b[29].high = 90.0;
        b[29].low = 88.0;
        let m = calculate(&b).unwrap();
        assert!(matches_setup("DARVAS_FAIL", &b, &m));
        assert!(matches_setup("RSI_OS", &b, &m));
        let mut b = bars(30);
        b[29].volume = 149.999;
        let m = calculate(&b).unwrap();
        assert!(!matches_setup("VOL_SURGE", &b, &m));
        b[29].volume = 150.0;
        assert!(matches_setup("VOL_SURGE", &b, &calculate(&b).unwrap()));
    }
    #[test]
    fn calendar_months_leap_year_year_boundary_ohlcv_and_incomplete_edges() {
        let input = vec![
            bar(d("2023-12-29"), 100.0),
            bar(d("2024-01-02"), 102.0),
            bar(d("2024-01-31"), 110.0),
            bar(d("2024-02-01"), 105.0),
            bar(d("2024-02-29"), 107.0),
            bar(d("2024-03-01"), 109.0),
        ];
        let months = completed_periods(&input, Timeframe::MN1);
        assert_eq!(months.len(), 2); // leading December and trailing March incomplete
        assert_eq!(date(months[0].ts), "2024-01-01");
        assert_eq!(date(months[1].ts), "2024-02-01");
        assert_eq!(
            (months[0].open, months[0].high, months[0].low, months[0].close, months[0].volume),
            (102.0, 111.0, 101.0, 110.0, 200.0)
        );
        assert_eq!(months[0].provenance, Provenance::Real);
        let b = vec![
            bar(d("2025-12-01"), 100.0),
            bar(d("2025-12-31"), 101.0),
            bar(d("2026-01-02"), 102.0),
            bar(d("2026-01-30"), 103.0),
        ];
        assert_eq!(
            completed_periods(&b, Timeframe::MN1).iter().map(|b| date(b.ts)).collect::<Vec<_>>(),
            vec!["2025-12-01", "2026-01-01"]
        );
        assert!(completed_periods(&[], Timeframe::MN1).is_empty());
    }
    #[test]
    fn weekly_calendar_holiday_completion_and_full_twenty_bar_lookback() {
        let b = vec![bar(d("2026-03-30"), 100.0), bar(d("2026-04-02"), 101.0)]; // Good Friday closed
        let w = completed_periods(&b, Timeframe::W1);
        assert_eq!(w.len(), 1);
        assert_eq!(date(w[0].ts), "2026-03-30");
        assert_eq!(w[0].volume, 200.0);
        assert!(completed_periods(&b[..1], Timeframe::W1).is_empty());
        let b = bars(150);
        let m = calculate(&b).unwrap();
        assert!(m.weekly.bars >= 20);
        assert_eq!(m.weekly.rsi, Some(50.0));
        assert_eq!(m.weekly.above_sma20, Some(true));
        let short = conditions(&b[..19], "monthly");
        assert_eq!(short.rsi, None);
        assert_eq!(short.above_sma20, None);
        assert!(short.unavailable.unwrap().contains("have 19"));
        assert!(conditions(&b[..20], "monthly").unavailable.is_none());
    }
    #[test]
    fn monthly_uses_all_daily_history_and_never_a_260_bar_tail() {
        let first = d("2022-01-03");
        let b: Vec<_> =
            (0..1100).map(|i| first + Days::days(i)).filter(|d| is_session(*d)).map(|d| bar(d, 100.0)).collect();
        let m = calculate(&b).unwrap();
        assert!(m.monthly.bars >= 35);
        assert_eq!(m.monthly.rsi, Some(50.0));
        assert!(m.monthly.unavailable.is_none());
        let tail = calculate(&b[b.len() - 260..]).unwrap();
        assert!(tail.monthly.unavailable.unwrap().contains("need 20"));
    }
    #[test]
    fn skip_reasons_for_missing_short_invalid_and_unsorted_history() {
        let valid = bars(30);
        let mut bad = valid.clone();
        bad[29].close = f64::NAN;
        let mut duplicate = valid.clone();
        duplicate[29].ts = duplicate[28].ts;
        let histories = vec![
            ("EMPTY".into(), vec![]),
            ("SHORT".into(), valid[..29].to_vec()),
            ("BAD".into(), bad),
            ("DUP".into(), duplicate),
            ("OK".into(), valid),
        ];
        let scan = ScanCache::default().scan("NEAR_HIGH", histories, Provenance::Cached).unwrap().0;
        assert_eq!(scan.scanned, 1);
        assert_eq!(scan.results.len(), 1);
        assert_eq!(scan.skipped.len(), 4);
        for (symbol, text) in [
            ("EMPTY", "missing daily"),
            ("SHORT", "need 30 bars, have 29"),
            ("BAD", "invalid archived"),
            ("DUP", "unique ascending"),
        ] {
            assert!(scan.skipped.iter().find(|s| s.symbol == symbol).unwrap().reason.contains(text));
        }
        let mut invalid = bars(30);
        invalid[0].volume = -1.0;
        assert!(calculate(&invalid).is_err());
    }
    #[test]
    fn cache_reuses_unchanged_bars_invalidates_corrections_and_refuses_samples() {
        let mut cache = ScanCache::default();
        let input = vec![("X".into(), bars(30))];
        assert!(!cache.scan("NEAR_HIGH", input.clone(), Provenance::Cached).unwrap().1);
        assert!(cache.scan("NEAR_HIGH", input.clone(), Provenance::Cached).unwrap().1);
        let mut revised = input.clone();
        revised[0].1[29].volume = 200.0;
        let (scan, hit) = cache.scan("NEAR_HIGH", revised.clone(), Provenance::Cached).unwrap();
        assert!(!hit);
        assert_eq!(scan.results[0].metrics.as_ref().unwrap().volume, 200.0);
        assert!(!cache.scan("NEAR_HIGH", revised.clone(), Provenance::Real).unwrap().1);
        assert!(!cache.scan("RSI_OB", revised.clone(), Provenance::Real).unwrap().1);
        revised[0].1[0].provenance = Provenance::Sample;
        assert!(cache.scan("NEAR_HIGH", revised, Provenance::Cached).unwrap_err().to_string().contains("refused"));
        assert!(cache.scan("NEAR_HIGH", input.clone(), Provenance::Sample).is_err());
        let mut sample = bars(30);
        for b in &mut sample {
            b.provenance = Provenance::Sample;
        }
        assert!(setup_active("NEAR_HIGH", &sample).unwrap_err().contains("SAMPLE"));
        assert!(setup_active("NEAR_HIGH", &bars(30)).unwrap());
        assert!(cache.scan("TYPO", input, Provenance::Cached).is_err());
        let mut cache = CalculationCache::default();
        let mut b = bars(30);
        let original = cache.get(&b).unwrap();
        assert_eq!(cache.entries.len(), 1);
        cache.get(&b).unwrap();
        assert_eq!(cache.entries.len(), 1);
        b[29].volume = 200.0;
        assert_ne!(cache.get(&b).unwrap().volume, original.volume);
        assert_eq!(cache.entries.len(), 2);
    }
    #[test]
    fn relative_strength_ties_same_date_cohorts_and_universe_before_filtering() {
        let mut a = bars(30);
        a[29].close = 100.5;
        let mut c = bars(30);
        c[29].close = 99.5;
        let mut old = bars(30);
        for b in &mut old {
            b.ts -= 86400;
        }
        let histories = vec![("A".into(), a.clone()), ("B".into(), a), ("C".into(), c), ("OLD".into(), old)];
        let scan = ScanCache::default().scan("NEAR_HIGH", histories, Provenance::Cached).unwrap().0;
        for r in &scan.results {
            let m = r.metrics.as_ref().unwrap();
            assert_eq!(
                (m.rs_rank, m.rs_n),
                match r.symbol.as_str() {
                    "A" | "B" => (1, 3),
                    "C" => (3, 3),
                    _ => (1, 1),
                }
            );
        }
    }
    #[test]
    fn every_numeric_filter_inclusive_bounds_missing_values_and_combination() {
        let r = row("X", 50.0);
        let expected = d("2026-10-21");
        let filters: Vec<Box<dyn Fn(&mut ScanFilter, f64)>> = vec![
            Box::new(|f, v| f.min_score = Some(v)),
            Box::new(|f, v| f.min_strength = Some(v)),
            Box::new(|f, v| f.min_rsi = Some(v)),
            Box::new(|f, v| f.min_volume = Some(v)),
            Box::new(|f, v| f.min_volume_ratio = Some(v)),
            Box::new(|f, v| f.min_range = Some(v)),
        ];
        for set in filters {
            let mut f = ScanFilter::default();
            set(&mut f, 50.0);
            assert!(f.reason(&r, expected).is_none());
            set(&mut f, 50.01);
            assert!(f.reason(&r, expected).unwrap().contains("outside filter"));
        }
        for range in [false, true] {
            let mut f = ScanFilter::default();
            if range {
                f.max_range = Some(50.0)
            } else {
                f.max_rsi = Some(50.0)
            };
            assert!(f.reason(&r, expected).is_none());
            if range {
                f.max_range = Some(49.0)
            } else {
                f.max_rsi = Some(49.0)
            };
            assert!(f.reason(&r, expected).is_some());
        }
        let mut f = ScanFilter {
            min_score: Some(10.0),
            min_rsi: Some(20.0),
            max_rsi: Some(60.0),
            min_volume: Some(40.0),
            min_range: Some(0.0),
            max_range: Some(100.0),
            ..Default::default()
        };
        assert!(f.reason(&r, expected).is_none());
        let mut missing = r.clone();
        missing.metrics = None;
        assert!(f.reason(&missing, expected).unwrap().contains("unavailable"));
        f = ScanFilter { min_volume_ratio: Some(1.0), ..Default::default() };
        missing.metrics = Some(Metrics::default());
        assert!(f.reason(&missing, expected).unwrap().contains("volume ratio unavailable"));
    }
    #[test]
    fn all_timeframe_conditions_and_insufficient_history_skip_reason() {
        let mut r = row("X", 75.0);
        let expected = d("2026-10-21");
        for cond in Condition::ALL {
            let mut f = ScanFilter { daily: cond, ..Default::default() };
            let accepted = matches!(cond, Condition::Any | Condition::AboveSma20 | Condition::Overbought);
            assert_eq!(f.reason(&r, expected).is_none(), accepted);
            f.daily = Condition::Any;
            f.weekly = cond;
            assert_eq!(f.reason(&r, expected).is_none(), accepted);
            f.weekly = Condition::Any;
            f.monthly = cond;
            assert_eq!(
                f.reason(&r, expected).is_none(),
                matches!(cond, Condition::Any | Condition::BelowSma20 | Condition::Oversold)
            );
        }
        r.metrics.as_mut().unwrap().monthly = conditions(&[], "completed monthly");
        let f = ScanFilter { monthly: Condition::AboveSma20, ..Default::default() };
        assert!(f.reason(&r, expected).unwrap().contains("insufficient completed monthly history"));
        r.metrics = None;
        assert!(f.reason(&r, expected).unwrap().contains("monthly conditions unavailable"));
    }
    #[test]
    fn all_sorts_both_directions_stable_ties_and_missing_last() {
        let mut missing = row("MISSING", 1.0);
        missing.metrics = None;
        let scan = Scan {
            provenance: Provenance::Real,
            scanned: 4,
            skipped: vec![],
            results: vec![row("B", 10.0), row("C", 20.0), row("A", 10.0), missing],
        };
        for sort in Sort::ALL {
            for descending in [false, true] {
                let f = ScanFilter { sort, descending, ..Default::default() };
                let names: Vec<_> = f.rows(&scan, "", d("2026-10-21")).iter().map(|r| r.symbol.as_str()).collect();
                let expected = if sort == Sort::Symbol {
                    if descending { vec!["MISSING", "C", "B", "A"] } else { vec!["A", "B", "C", "MISSING"] }
                } else if descending {
                    vec!["C", "A", "B", "MISSING"]
                } else {
                    vec!["A", "B", "C", "MISSING"]
                };
                assert_eq!(names, expected, "{sort:?}, {descending}");
                assert_eq!(f.rows(&scan, " b ", d("2026-10-21"))[0].symbol, "B");
            }
        }
    }
    #[test]
    fn staleness_defaults_toggle_missing_and_session_close_rollover() {
        let r = row("X", 50.0);
        let mut f = ScanFilter::default();
        assert!(!f.include_stale);
        assert!(f.reason(&r, d("2026-10-21")).is_none());
        assert!(f.reason(&r, d("2026-10-22")).unwrap().contains("stale: last bar 2026-10-21"));
        f.include_stale = true;
        assert!(f.reason(&r, d("2026-10-22")).is_none());
        let mut missing = r.clone();
        missing.last_date = None;
        f.include_stale = false;
        assert!(f.reason(&missing, d("2026-10-21")).unwrap().contains("missing last"));
        missing.last_date = Some("invalid".into());
        assert!(f.reason(&missing, d("2026-10-21")).is_some());
        f.include_stale = true;
        assert!(f.reason(&missing, d("2026-10-21")).is_none());
        let expected = |s: &str| crate::history::calendar::last_expected_session(s.parse().unwrap());
        assert_eq!(expected("2026-10-22T19:59:00Z"), d("2026-10-21"));
        assert_eq!(expected("2026-10-22T20:16:00Z"), d("2026-10-22"));
        assert_eq!(expected("2026-07-05T16:00:00Z"), d("2026-07-02"));
    }
    #[test]
    fn invalid_dates_and_calculation_overflow_skip_with_a_reason() {
        let mut b = bars(30);
        b[29].ts = i64::MAX;
        assert!(calculate(&b).unwrap_err().contains("invalid archived OHLCV or date"));
        let mut b = bars(30);
        for bar in &mut b {
            bar.volume = 1e-300;
        }
        b[29].volume = 1e300;
        assert!(calculate(&b).unwrap_err().contains("non-finite calculation"));
    }
}
