//! Equity scan-view engine: Pattern (daily 63/21-bar, weekly 52/26-week extremes) and the
//! RSI Counter (RSI lengths 2–21 on the same last bar), daily and completed-weekly.
//!
//! One board per (history content, params): every row carries both views' reads, so both
//! panes read the same cached board. Deliberate divergences from the Python reference:
//! * weekly bars are *completed* weeks only (reference resamples including the partial week);
//! * classification uses unrounded RSI (the reference rounds 19.995 to 20.00, which is not <20; we count [19.995, 20) as <20).
//!
//! The reference RSI here (α = 1/n EWM, first-delta seed, no-loss window reads 100) is
//! intentionally separate from `scanner::rsi` (Wilder SMA seed, flat reads 50).
//! No wall clock anywhere: keys and compute are pure functions of bar content.
use super::scanner::{completed_periods, validate_ohlc};
use super::Skipped;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tr_core::{Bar, Timeframe};

#[cfg(test)]
mod tests;

pub const ENGINE_METRIC_VERSION: u32 = 1;
/// Consumed daily bars (>= 120 completed weeks).
pub const DAILY_TAIL: usize = 650;
pub const RSI_DAILY_WINDOW: usize = 400;
pub const RSI_WEEKLY_WINDOW: usize = 120;
pub const RSIC_MIN_CLOSES: usize = 24;
pub const RSIC_MIN_VALID: usize = 10;
pub const DAILY_LONG: usize = 63;
pub const DAILY_SHORT: usize = 21;
pub const WEEKLY_YEAR: usize = 52;
pub const WEEKLY_HALF: usize = 26;
pub const WEEKLY_MIN: usize = 28;
pub const WEEKLY_EPS: f64 = 0.001;
/// Extreme / Strong / Tilt thresholds in percent of valid lengths (integer arithmetic only).
pub const TIERS_PCT: [usize; 3] = [90, 70, 55];
const THIRD: f64 = 1.0 / 3.0;
const TWO_THIRDS: f64 = 2.0 / 3.0;
const BOARD_CACHE_CAP: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScanView {
    Pattern,
    RsiCounter,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EngineParams {
    pub rsi_n: usize,
    pub lag: usize,
}
impl Default for EngineParams {
    fn default() -> Self {
        Self { rsi_n: 14, lag: 5 }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanViewSpec {
    pub view: ScanView,
    pub params: EngineParams,
}
impl ScanViewSpec {
    /// `None` when the first word isn't `VIEW` (ordinary setup names are untouched).
    pub fn parse(scan: &str) -> Option<Result<Self, String>> {
        let mut words = scan.split_whitespace();
        if !words.next()?.eq_ignore_ascii_case("VIEW") {
            return None;
        }
        Some(Self::parse_rest(words))
    }
    fn parse_rest<'a>(mut words: impl Iterator<Item = &'a str>) -> Result<Self, String> {
        let Some(name) = words.next() else {
            return Err("SCAN VIEW needs PATTERN or RSI-C".into());
        };
        let view = match name.to_ascii_uppercase().as_str() {
            "PATTERN" => ScanView::Pattern,
            "RSI-C" | "RSIC" => ScanView::RsiCounter,
            other => return Err(format!("Unknown view {other}. Available: PATTERN, RSI-C")),
        };
        let mut params = EngineParams::default();
        let (mut seen_n, mut seen_k) = (false, false);
        for word in words {
            if view == ScanView::Pattern {
                return Err(format!("PATTERN takes no parameters (got {word})"));
            }
            let upper = word.to_ascii_uppercase();
            let Some((key, value)) = upper.split_once('=') else {
                return Err(format!("Bad parameter {word}. Use N=<2..21> K=<1..20>"));
            };
            let parsed = value.parse::<usize>().map_err(|_| format!("{key} needs a whole number, got {value:?}"))?;
            match key {
                "N" => {
                    if std::mem::replace(&mut seen_n, true) {
                        return Err("N given twice".into());
                    }
                    if !(2..=21).contains(&parsed) {
                        return Err(format!("N must be 2..21, got {parsed}"));
                    }
                    params.rsi_n = parsed;
                }
                "K" => {
                    if std::mem::replace(&mut seen_k, true) {
                        return Err("K given twice".into());
                    }
                    if !(1..=20).contains(&parsed) {
                        return Err(format!("K must be 1..20, got {parsed}"));
                    }
                    params.lag = parsed;
                }
                _ => return Err(format!("Unknown parameter {key}. Use N=<2..21> K=<1..20>")),
            }
        }
        Ok(Self { view, params })
    }
    /// Persisted form: non-default params only.
    pub fn canonical(&self) -> String {
        match self.view {
            ScanView::Pattern => "VIEW PATTERN".into(),
            ScanView::RsiCounter => {
                let d = EngineParams::default();
                let mut s = String::from("VIEW RSI-C");
                if self.params.rsi_n != d.rsi_n {
                    s += &format!(" N={}", self.params.rsi_n);
                }
                if self.params.lag != d.lag {
                    s += &format!(" K={}", self.params.lag);
                }
                s
            }
        }
    }
    pub fn describe(&self) -> String {
        match self.view {
            ScanView::Pattern => "Scanner view: Pattern — 63/21-bar daily and 52/26-week extremes, thirds".into(),
            ScanView::RsiCounter => format!(
                "Scanner view: RSI Counter — RSI 2–21 on the last daily and completed weekly bar; Δ RSI{}/{}",
                self.params.rsi_n, self.params.lag
            ),
        }
    }
}
impl ScanView {
    pub fn title(self) -> &'static str {
        match self {
            Self::Pattern => "Pattern",
            Self::RsiCounter => "RSI Counter",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PatternKind {
    Breakout,
    Breakdown,
    FromBottom,
    FromTop,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Unavailable {
    Short { have: usize, need: usize },
    TooFewLengths { valid: usize },
    Invalid(String),
}
impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Short { have, need } => write!(f, "need {need} bars, have {have}"),
            Self::TooFewLengths { valid } => write!(f, "only {valid} valid RSI lengths (need {RSIC_MIN_VALID})"),
            Self::Invalid(why) => f.write_str(why),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatternRead {
    pub kind: Option<PatternKind>,
    pub close: f64,
    pub hi_long: f64,
    pub lo_long: f64,
    pub hi_short: f64,
    pub lo_short: f64,
    /// (C−L)/(H−L), raw (may be ≥1/≤0 on breaks); `None` iff H == L.
    pub position: Option<f64>,
    /// Daily 63; weekly actual long-window weeks 28..=52.
    pub bars_used: usize,
    /// Weekly: bars_used == 52; daily: true.
    pub full_year: bool,
    /// DAILY bar ts to open.
    pub trigger_ts: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RsicState {
    OsExtreme,
    Oversold,
    OsLean,
    ObExtreme,
    Overbought,
    ObLean,
    UpStrong,
    Up,
    UpTilt,
    DownStrong,
    Down,
    DownTilt,
    Mixed,
}
impl RsicState {
    pub fn label(self) -> &'static str {
        match self {
            Self::OsExtreme => "OS EXTREME",
            Self::Oversold => "OVERSOLD",
            Self::OsLean => "OS LEAN",
            Self::ObExtreme => "OB EXTREME",
            Self::Overbought => "OVERBOUGHT",
            Self::ObLean => "OB LEAN",
            Self::UpStrong => "TREND ↑ STRONG",
            Self::Up => "TREND ↑",
            Self::UpTilt => "TILT ↑",
            Self::DownStrong => "TREND ↓ STRONG",
            Self::Down => "TREND ↓",
            Self::DownTilt => "TILT ↓",
            Self::Mixed => "MIXED",
        }
    }
    pub fn is_os(self) -> bool {
        matches!(self, Self::OsExtreme | Self::Oversold | Self::OsLean)
    }
    pub fn is_ob(self) -> bool {
        matches!(self, Self::ObExtreme | Self::Overbought | Self::ObLean)
    }
    pub fn is_up(self) -> bool {
        matches!(self, Self::UpStrong | Self::Up | Self::UpTilt)
    }
    pub fn is_down(self) -> bool {
        matches!(self, Self::DownStrong | Self::Down | Self::DownTilt)
    }
    /// Bullish-to-bearish ordering (overbought/up high, oversold/down low) for sorting.
    pub fn sort_key(self) -> i8 {
        match self {
            Self::ObExtreme => 12,
            Self::Overbought => 11,
            Self::ObLean => 10,
            Self::UpStrong => 9,
            Self::Up => 8,
            Self::UpTilt => 7,
            Self::Mixed => 6,
            Self::DownTilt => 5,
            Self::Down => 4,
            Self::DownStrong => 3,
            Self::OsLean => 2,
            Self::Oversold => 1,
            Self::OsExtreme => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RsiCounter {
    /// Index i ↔ RSI length i+2, unrounded.
    pub values: [Option<f64>; 20],
    pub valid: usize,
    pub os20: usize,
    pub os10: usize,
    pub ob80: usize,
    pub ob90: usize,
    pub up: usize,
    pub down: usize,
    pub mean: f64,
    pub sd: f64,
    pub align: f64,
    pub state: RsicState,
    /// All deltas in the window are zero (reference reads 100 there).
    pub flat: bool,
    pub closes_used: usize,
}
/// 0 = Extreme, 1 = Strong, 2 = Tilt; integer arithmetic only.
fn tier(count: usize, valid: usize) -> Option<usize> {
    TIERS_PCT.iter().position(|pct| 100 * count >= pct * valid)
}
impl RsiCounter {
    pub fn from_values(values: [Option<f64>; 20], closes_used: usize, flat: bool) -> Result<Self, Unavailable> {
        let v: Vec<f64> = values.iter().flatten().copied().filter(|x| x.is_finite()).collect();
        let valid = v.len();
        if valid < RSIC_MIN_VALID {
            return Err(Unavailable::TooFewLengths { valid });
        }
        let count = |f: &dyn Fn(f64) -> bool| v.iter().filter(|x| f(**x)).count();
        let os20 = count(&|x| x < 20.0);
        let os10 = count(&|x| x < 10.0);
        let ob80 = count(&|x| x > 80.0);
        let ob90 = count(&|x| x > 90.0);
        let up = count(&|x| (50.0..=75.0).contains(&x));
        let down = count(&|x| (25.0..=50.0).contains(&x));
        let mean = v.iter().sum::<f64>() / valid as f64;
        let sd = (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (valid - 1) as f64).sqrt();
        let align = 1.0 - (sd / 25.0).min(1.0);
        let state = if let Some(t) = tier(os20, valid) {
            [RsicState::OsExtreme, RsicState::Oversold, RsicState::OsLean][t]
        } else if let Some(t) = tier(ob80, valid) {
            [RsicState::ObExtreme, RsicState::Overbought, RsicState::ObLean][t]
        } else if let Some(t) = tier(up, valid) {
            [RsicState::UpStrong, RsicState::Up, RsicState::UpTilt][t]
        } else if let Some(t) = tier(down, valid) {
            [RsicState::DownStrong, RsicState::Down, RsicState::DownTilt][t]
        } else {
            RsicState::Mixed
        };
        Ok(Self { values, valid, os20, os10, ob80, ob90, up, down, mean, sd, align, state, flat, closes_used })
    }
    pub fn share(&self, count: usize) -> f64 {
        count as f64 / self.valid as f64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Agreement {
    Up,
    Down,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeeklyRead {
    pub week_start: i64,
    pub trigger_ts: i64,
    pub weeks: usize,
    pub pattern: Result<PatternRead, Unavailable>,
    pub rsic: Result<RsiCounter, Unavailable>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineRow {
    pub symbol: String,
    pub last_ts: i64,
    pub last_date: String,
    pub close: f64,
    pub pattern_d: Result<PatternRead, Unavailable>,
    pub rsic_d: Result<RsiCounter, Unavailable>,
    /// RSI_n(t) − RSI_n(t−k), reference EWM, unrounded.
    pub accel: Option<f64>,
    /// A weekly failure never removes the row.
    pub weekly: Result<WeeklyRead, Unavailable>,
    pub agreement: Option<Agreement>,
    pub pullback: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EngineBoard {
    pub params: EngineParams,
    pub metric_version: u32,
    /// Sorted by symbol.
    pub rows: Vec<EngineRow>,
    pub short: Vec<Skipped>,
    pub invalid: Vec<Skipped>,
    /// Max row `last_ts` (data-derived, no clock).
    pub as_of_ts: Option<i64>,
    /// Max weekly `week_start`.
    pub as_of_week: Option<i64>,
}

/// Reference EWM RSI on exactly the closes given (callers apply the 400/120 windows).
/// α = 1/n, first-delta seed; defined only for len ≥ n+2; a window with no loss reads 100.
pub fn rsi_reference_ewm(closes: &[f64], n: usize) -> Option<f64> {
    if n == 0 || closes.len() < n + 2 {
        return None;
    }
    let alpha = 1.0 / n as f64;
    let d1 = closes[1] - closes[0];
    let (mut gain, mut loss) = (d1.max(0.0), (-d1).max(0.0));
    for w in closes[1..].windows(2) {
        let d = w[1] - w[0];
        gain = (1.0 - alpha) * gain + alpha * d.max(0.0);
        loss = (1.0 - alpha) * loss + alpha * (-d).max(0.0);
    }
    let v = if loss > 0.0 { 100.0 - 100.0 / (1.0 + gain / loss) } else { 100.0 };
    v.is_finite().then_some(v)
}

/// RSI lengths 2–21 on the same closes (already windowed by the caller).
pub fn rsi_counter(closes: &[f64]) -> Result<RsiCounter, Unavailable> {
    if closes.len() < RSIC_MIN_CLOSES {
        return Err(Unavailable::Short { have: closes.len(), need: RSIC_MIN_CLOSES });
    }
    let mut values = [None; 20];
    for (i, slot) in values.iter_mut().enumerate() {
        *slot = rsi_reference_ewm(closes, i + 2);
    }
    let flat = closes.windows(2).all(|w| w[1] == w[0]);
    RsiCounter::from_values(values, closes.len(), flat)
}

fn window_max(bars: &[Bar]) -> f64 {
    bars.iter().map(|b| b.high).fold(f64::MIN, f64::max)
}
fn window_min(bars: &[Bar]) -> f64 {
    bars.iter().map(|b| b.low).fold(f64::MAX, f64::min)
}

/// Daily pattern against the *prior* 63/21-bar windows (they end at t−1).
pub fn pattern_daily(bars: &[Bar]) -> Result<PatternRead, Unavailable> {
    let need = DAILY_LONG + 1;
    if bars.len() < need {
        return Err(Unavailable::Short { have: bars.len(), need });
    }
    let t = bars.len() - 1;
    let long = &bars[t - DAILY_LONG..t];
    let short = &bars[t - DAILY_SHORT..t];
    let (h, l, h21, l21) = (window_max(long), window_min(long), window_max(short), window_min(short));
    let c = bars[t].close;
    let position = (h > l).then(|| (c - l) / (h - l));
    let kind = if c >= h {
        Some(PatternKind::Breakout)
    } else if c <= l {
        Some(PatternKind::Breakdown)
    } else {
        // c is strictly inside (l, h), so h > l and position is Some.
        let p = position.unwrap_or(0.5);
        if c >= h21 && p <= THIRD {
            Some(PatternKind::FromBottom)
        } else if c <= l21 && p >= TWO_THIRDS {
            Some(PatternKind::FromTop)
        } else {
            None
        }
    };
    Ok(PatternRead {
        kind,
        close: c,
        hi_long: h,
        lo_long: l,
        hi_short: h21,
        lo_short: l21,
        position,
        bars_used: DAILY_LONG,
        full_year: true,
        trigger_ts: bars[t].ts,
    })
}

/// Weekly pattern over completed weeks; windows include the last completed week.
pub fn pattern_weekly(weeks: &[Bar], trigger_ts: i64) -> Result<PatternRead, Unavailable> {
    let weeks = &weeks[weeks.len().saturating_sub(RSI_WEEKLY_WINDOW)..];
    let m = weeks.len();
    if m < WEEKLY_MIN {
        return Err(Unavailable::Short { have: m, need: WEEKLY_MIN });
    }
    let w = m.min(WEEKLY_YEAR);
    let long = &weeks[m - w..];
    let short = &weeks[m - WEEKLY_HALF..];
    let (h, l, h26, l26) = (window_max(long), window_min(long), window_max(short), window_min(short));
    let c = weeks[m - 1].close;
    let position = (h > l).then(|| (c - l) / (h - l));
    let kind = if c >= h * (1.0 - WEEKLY_EPS) {
        Some(PatternKind::Breakout)
    } else if c <= l * (1.0 + WEEKLY_EPS) {
        Some(PatternKind::Breakdown)
    } else if let Some(p) = position {
        if c >= h26 * (1.0 - WEEKLY_EPS) && p <= THIRD {
            Some(PatternKind::FromBottom)
        } else if c <= l26 * (1.0 + WEEKLY_EPS) && p >= TWO_THIRDS {
            Some(PatternKind::FromTop)
        } else {
            None
        }
    } else {
        None
    };
    Ok(PatternRead {
        kind,
        close: c,
        hi_long: h,
        lo_long: l,
        hi_short: h26,
        lo_short: l26,
        position,
        bars_used: w,
        full_year: w == WEEKLY_YEAR,
        trigger_ts,
    })
}

fn closes_window(bars: &[Bar], window: usize) -> Vec<f64> {
    bars[bars.len().saturating_sub(window)..].iter().map(|b| b.close).collect()
}
/// D/W agreement (both up or both down) and pullback (daily oversold inside a weekly uptrend).
pub fn dw_flags(d: RsicState, w: RsicState) -> (Option<Agreement>, bool) {
    let agreement = if d.is_up() && w.is_up() {
        Some(Agreement::Up)
    } else if d.is_down() && w.is_down() {
        Some(Agreement::Down)
    } else {
        None
    };
    (agreement, d.is_os() && w.is_up())
}
fn date_label(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).map(|t| t.date_naive().to_string()).unwrap_or_default()
}

/// (is_short, reason) — `short` rows go to `EngineBoard::short`, the rest to `invalid`.
type RowError = (bool, Skipped);

fn try_row(symbol: &str, bars: &[Bar], p: EngineParams) -> Result<EngineRow, RowError> {
    let skip = |short: bool, reason: String| (short, Skipped { symbol: symbol.into(), reason });
    if bars.is_empty() {
        return Err(skip(false, "missing daily history".into()));
    }
    let tail = &bars[bars.len() - bars.len().min(DAILY_TAIL)..];
    if !Bar::provenance(tail).is_verified() {
        return Err(skip(false, "unverified history".into()));
    }
    validate_ohlc(tail, false).map_err(|e| skip(false, e))?;
    if tail.len() < RSIC_MIN_CLOSES {
        return Err(skip(
            true,
            format!("insufficient daily history: need {RSIC_MIN_CLOSES} bars, have {}", tail.len()),
        ));
    }
    let last = tail[tail.len() - 1];
    let closes = closes_window(tail, RSI_DAILY_WINDOW);
    let rsic_d = rsi_counter(&closes);
    let accel = (|| {
        let k = p.lag;
        let cut = closes.len().checked_sub(k)?;
        Some(rsi_reference_ewm(&closes, p.rsi_n)? - rsi_reference_ewm(&closes[..cut], p.rsi_n)?)
    })();
    let weeks = completed_periods(tail, Timeframe::W1);
    let weekly = match weeks.last() {
        None => Err(Unavailable::Short { have: 0, need: WEEKLY_MIN }),
        Some(wk) => {
            let week_end = wk.ts + 7 * 86_400;
            let trigger_ts = tail.iter().rev().find(|b| b.ts >= wk.ts && b.ts < week_end).map_or(wk.ts, |b| b.ts);
            let used = &weeks[weeks.len().saturating_sub(RSI_WEEKLY_WINDOW)..];
            Ok(WeeklyRead {
                week_start: wk.ts,
                trigger_ts,
                weeks: used.len(),
                pattern: pattern_weekly(used, trigger_ts),
                rsic: rsi_counter(&closes_window(used, RSI_WEEKLY_WINDOW)),
            })
        }
    };
    let (agreement, pullback) = match (&rsic_d, &weekly) {
        (Ok(d), Ok(WeeklyRead { rsic: Ok(w), .. })) => dw_flags(d.state, w.state),
        _ => (None, false),
    };
    Ok(EngineRow {
        symbol: symbol.into(),
        last_ts: last.ts,
        last_date: date_label(last.ts),
        close: last.close,
        pattern_d: pattern_daily(tail),
        rsic_d,
        accel,
        weekly,
        agreement,
        pullback,
    })
}

pub fn engine_row(symbol: &str, bars: &[Bar], p: EngineParams) -> Result<EngineRow, Skipped> {
    try_row(symbol, bars, p).map_err(|(_, s)| s)
}

/// Any Sample bar anywhere refuses the whole board (callers surface the error).
pub fn refuse_sample(histories: &[(String, Vec<Bar>)]) -> Result<(), String> {
    for (symbol, bars) in histories {
        if bars.iter().any(|b| b.provenance == tr_core::Provenance::Sample) {
            return Err(format!("{symbol}: sample bars refused — scanner views use verified archive bars only"));
        }
    }
    Ok(())
}

pub fn engine_board(histories: &[(String, Vec<Bar>)], p: EngineParams) -> EngineBoard {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(4);
    engine_board_threads(histories, p, threads)
}

/// Deterministic across thread counts: chunks merge in input order, then sort by symbol.
pub fn engine_board_threads(histories: &[(String, Vec<Bar>)], p: EngineParams, threads: usize) -> EngineBoard {
    let run = |chunk: &[(String, Vec<Bar>)]| -> Vec<Result<EngineRow, RowError>> {
        chunk.iter().map(|(s, b)| try_row(s, b, p)).collect()
    };
    let threads = threads.clamp(1, 16);
    let results: Vec<Result<EngineRow, RowError>> = if threads == 1 || histories.len() < 2 * threads {
        run(histories)
    } else {
        let size = histories.len().div_ceil(threads);
        std::thread::scope(|scope| {
            let handles: Vec<_> = histories.chunks(size).map(|c| scope.spawn(move || run(c))).collect();
            handles.into_iter().flat_map(|h| h.join().expect("engine worker panicked")).collect()
        })
    };
    let mut board = EngineBoard { params: p, metric_version: ENGINE_METRIC_VERSION, ..Default::default() };
    for r in results {
        match r {
            Ok(row) => board.rows.push(row),
            Err((true, s)) => board.short.push(s),
            Err((false, s)) => board.invalid.push(s),
        }
    }
    board.rows.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    board.short.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    board.invalid.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    board.as_of_ts = board.rows.iter().map(|r| r.last_ts).max();
    board.as_of_week = board.rows.iter().filter_map(|r| r.weekly.as_ref().ok().map(|w| w.week_start)).max();
    board
}

/// Content key: version + params + count + two independent 64-bit digests over the sorted
/// (symbol, tail length, tail bars). The digests are FNV-style and SipHash over 64-bit words.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BoardKey {
    version: u32,
    params: EngineParams,
    n: usize,
    fnv: u64,
    sip: u64,
}
pub fn board_key(h: &[(String, Vec<Bar>)], p: EngineParams) -> BoardKey {
    use std::hash::Hasher;
    let mut order: Vec<usize> = (0..h.len()).collect();
    order.sort_by(|a, b| h[*a].0.cmp(&h[*b].0));
    let mut fnv = 0xcbf2_9ce4_8422_2325u64;
    let mut sip = std::collections::hash_map::DefaultHasher::new();
    let mut word = |w: u64| {
        fnv = (fnv ^ w).wrapping_mul(0x0000_0100_0000_01b3);
        fnv ^= fnv >> 29;
        sip.write_u64(w);
    };
    for i in order {
        let (symbol, bars) = &h[i];
        let tail = &bars[bars.len() - bars.len().min(DAILY_TAIL)..];
        word(symbol.len() as u64);
        for chunk in symbol.as_bytes().chunks(8) {
            let mut buf = [0u8; 8];
            buf[..chunk.len()].copy_from_slice(chunk);
            word(u64::from_le_bytes(buf));
        }
        word(tail.len() as u64);
        for b in tail {
            word(b.ts as u64);
            word(b.open.to_bits());
            word(b.high.to_bits());
            word(b.low.to_bits());
            word(b.close.to_bits());
            word(b.volume.to_bits());
            word(u64::from(b.provenance.is_verified()));
        }
    }
    BoardKey { version: ENGINE_METRIC_VERSION, params: p, n: h.len(), fnv, sip: sip.finish() }
}

/// Bounded LRU of computed boards. The lock is never held while computing.
#[derive(Default)]
pub struct BoardCache {
    inner: Mutex<(Vec<(BoardKey, Arc<EngineBoard>, u64)>, u64)>,
}
impl BoardCache {
    pub fn get_or_compute(&self, h: &[(String, Vec<Bar>)], p: EngineParams) -> (Arc<EngineBoard>, bool) {
        self.get_or_compute_with(h, p, || engine_board(h, p))
    }
    pub fn get_or_compute_with(
        &self,
        h: &[(String, Vec<Bar>)],
        p: EngineParams,
        compute: impl FnOnce() -> EngineBoard,
    ) -> (Arc<EngineBoard>, bool) {
        let key = board_key(h, p);
        {
            let mut g = self.inner.lock().unwrap();
            g.1 += 1;
            let tick = g.1;
            if let Some(e) = g.0.iter_mut().find(|e| e.0 == key) {
                e.2 = tick;
                return (e.1.clone(), true);
            }
        }
        let board = Arc::new(compute());
        let mut g = self.inner.lock().unwrap();
        g.1 += 1;
        let tick = g.1;
        if let Some(e) = g.0.iter_mut().find(|e| e.0 == key) {
            e.2 = tick;
        } else {
            if g.0.len() >= BOARD_CACHE_CAP {
                if let Some(oldest) = g.0.iter().enumerate().min_by_key(|(_, e)| e.2).map(|(i, _)| i) {
                    g.0.remove(oldest);
                }
            }
            g.0.push((key, board.clone(), tick));
        }
        (board, false)
    }
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// True when no one holds the lock (tests assert this during compute).
    pub fn lock_free(&self) -> bool {
        self.inner.try_lock().is_ok()
    }
}

/// Finite keys first in the requested direction, `None`/NaN last in both directions,
/// ties by symbol ascending (same rules as `ScanFilter::rows`).
pub fn sort_order<K: Fn(&EngineRow) -> Option<f64>>(rows: &[EngineRow], idx: &[usize], key: K, desc: bool) -> Vec<usize> {
    let mut keyed: Vec<(usize, Option<f64>)> = idx.iter().map(|&i| (i, key(&rows[i]).filter(|v| v.is_finite()))).collect();
    keyed.sort_by(|a, b| {
        use std::cmp::Ordering::*;
        let by_key = match (a.1, b.1) {
            (Some(x), Some(y)) => {
                if desc {
                    y.total_cmp(&x)
                } else {
                    x.total_cmp(&y)
                }
            }
            (Some(_), None) => Less,
            (None, Some(_)) => Greater,
            (None, None) => Equal,
        };
        by_key.then_with(|| rows[a.0].symbol.cmp(&rows[b.0].symbol))
    });
    keyed.into_iter().map(|(i, _)| i).collect()
}
