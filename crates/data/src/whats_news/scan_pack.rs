//! Pure daily MA lens and universe breadth, ported from Whats-News scan_pack.py.
//! Input order is oldest to newest. Missing values retain their positions.
use serde::{Deserialize, Serialize};

pub const SMA_WINDOWS: [usize; 3] = [20, 50, 200];
pub const PULLBACK_LO: f64 = -3.0;
pub const PULLBACK_HI: f64 = 0.5;

fn finite(x: f64) -> Option<f64> {
    x.is_finite().then_some(x)
}
fn rounded(x: f64, digits: i32) -> Option<f64> {
    // Formatting uses ties-to-even decimal rounding of the original binary float,
    // as Python round does (multiplying first can change boundary cases).
    finite(x).and_then(|x| format!("{x:.precision$}", precision = digits as usize).parse().ok())
}
pub fn sma(close: &[f64], n: usize) -> Option<f64> {
    if n == 0 || close.len() < n {
        return None;
    }
    let mean = close[close.len() - n..].iter().sum::<f64>() / n as f64;
    finite(mean).filter(|v| *v > 0.0)
}
pub fn sma_prev(close: &[f64], n: usize) -> Option<f64> {
    close.get(..close.len().checked_sub(1)?).and_then(|c| sma(c, n))
}
pub fn rising(now: Option<f64>, prev: Option<f64>) -> Option<bool> {
    Some(finite(now?)? > finite(prev?)?)
}
pub fn vs(close: Option<f64>, average: Option<f64>) -> Option<f64> {
    let c = finite(close?)?;
    let s = finite(average?)?;
    if s <= 0.0 {
        return None;
    }
    rounded((c / s - 1.0) * 100.0, 2)
}
pub fn in_band(value: Option<f64>) -> bool {
    value.and_then(finite).is_some_and(|v| (PULLBACK_LO..=PULLBACK_HI).contains(&v))
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MaMetrics {
    pub ready: bool,
    pub price: Option<f64>,
    pub day_pct: Option<f64>,
    pub bars: usize,
    pub sma20: Option<f64>,
    pub sma50: Option<f64>,
    pub sma200: Option<f64>,
    pub sma20_rising: Option<bool>,
    pub sma50_rising: Option<bool>,
    pub sma200_rising: Option<bool>,
    pub vs20: Option<f64>,
    pub vs50: Option<f64>,
    pub vs200: Option<f64>,
    pub stacked_ma: bool,
    pub pullback_rising_ma: bool,
}
impl MaMetrics {
    pub fn matches_ma(&self) -> bool {
        self.stacked_ma || self.pullback_rising_ma
    }
}
pub fn measure(close: &[f64]) -> MaMetrics {
    let last = close.last().copied().and_then(finite);
    let prev = close.len().checked_sub(2).and_then(|i| finite(close[i]));
    let averages = SMA_WINDOWS.map(|n| sma(close, n));
    let slopes = std::array::from_fn::<_, 3, _>(|i| rising(averages[i], sma_prev(close, SMA_WINDOWS[i])));
    let distances = averages.map(|s| vs(last, s));
    let stacked = match (last, averages[0], averages[1], averages[2]) {
        (Some(c), Some(a), Some(b), Some(d)) => c > a && a > b && b > d,
        _ => false,
    };
    MaMetrics {
        ready: last.is_some(),
        price: last.and_then(|v| rounded(v, 4)),
        day_pct: last
            .zip(prev)
            .filter(|(a, b)| *a != 0.0 && *b != 0.0)
            .and_then(|(a, b)| rounded((a / b - 1.0) * 100.0, 2)),
        bars: close.iter().filter(|v| !v.is_nan()).count(),
        sma20: averages[0].and_then(|v| rounded(v, 4)),
        sma50: averages[1].and_then(|v| rounded(v, 4)),
        sma200: averages[2].and_then(|v| rounded(v, 4)),
        sma20_rising: slopes[0],
        sma50_rising: slopes[1],
        sma200_rising: slopes[2],
        vs20: distances[0],
        vs50: distances[1],
        vs200: distances[2],
        stacked_ma: stacked,
        pullback_rising_ma: (slopes[0] == Some(true) && in_band(distances[0]))
            || (slopes[1] == Some(true) && in_band(distances[1])),
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Breadth {
    pub ready: bool,
    pub n: usize,
    pub stored_n: usize,
    pub n_sma50: usize,
    pub n_sma200: usize,
    pub n_ad_1d: usize,
    pub n_ad_5d: usize,
    pub pct_above_sma50: Option<f64>,
    pub pct_above_sma200: Option<f64>,
    pub adv_1d: Option<usize>,
    pub dec_1d: Option<usize>,
    pub unch_1d: Option<usize>,
    pub adv_5d: Option<usize>,
    pub dec_5d: Option<usize>,
    pub unch_5d: Option<usize>,
    pub ad_1d: Option<f64>,
    pub ad_5d: Option<f64>,
}
/// Rows must include the full universe before MA/setup/text filtering. No I/O.
pub fn breadth_from_rows<'a>(rows: impl IntoIterator<Item = (&'a MaMetrics, &'a [f64])>) -> Breadth {
    let mut b = Breadth::default();
    let (mut above50, mut above200) = (0, 0);
    let (mut one, mut five) = ([0usize; 3], [0usize; 3]);
    let tally = |v: f64, counts: &mut [usize; 3]| {
        counts[if v > 0.0 {
            0
        } else if v < 0.0 {
            1
        } else {
            2
        }] += 1;
    };
    for (row, close) in rows {
        if !row.ready {
            continue;
        }
        b.n += 1;
        if let Some(v) = row.vs50 {
            b.n_sma50 += 1;
            above50 += usize::from(v > 0.0);
        }
        if let Some(v) = row.vs200 {
            b.n_sma200 += 1;
            above200 += usize::from(v > 0.0);
        }
        if let Some(v) = row.day_pct.and_then(finite) {
            tally(v, &mut one);
        }
        if close.iter().filter(|v| !v.is_nan()).count() >= 6 && close.len() >= 6 {
            if let (Some(c), Some(p)) = (finite(close[close.len() - 1]), finite(close[close.len() - 6])) {
                if p > 0.0 {
                    tally(c - p, &mut five);
                }
            }
        }
    }
    b.ready = b.n > 0;
    b.stored_n = b.n;
    b.n_ad_1d = one.iter().sum();
    b.n_ad_5d = five.iter().sum();
    if b.n_sma50 > 0 {
        b.pct_above_sma50 = rounded(above50 as f64 / b.n_sma50 as f64 * 100.0, 1);
    }
    if b.n_sma200 > 0 {
        b.pct_above_sma200 = rounded(above200 as f64 / b.n_sma200 as f64 * 100.0, 1);
    }
    if b.n_ad_1d > 0 {
        b.adv_1d = Some(one[0]);
        b.dec_1d = Some(one[1]);
        b.unch_1d = Some(one[2]);
    }
    if b.n_ad_5d > 0 {
        b.adv_5d = Some(five[0]);
        b.dec_5d = Some(five[1]);
        b.unch_5d = Some(five[2]);
    }
    if one[0] + one[1] > 0 {
        b.ad_1d = rounded(one[0] as f64 / (one[0] + one[1]) as f64, 3);
    }
    if five[0] + five[1] > 0 {
        b.ad_5d = rounded(five[0] as f64 / (five[0] + five[1]) as f64, 3);
    }
    b
}
pub fn breadth(histories: &[Vec<f64>]) -> Breadth {
    let rows: Vec<_> = histories.iter().map(|c| measure(c)).collect();
    breadth_from_rows(rows.iter().zip(histories.iter().map(Vec::as_slice)))
}
impl Breadth {
    pub fn summary(&self) -> String {
        let num = |v: Option<f64>| v.map(|v| v.to_string()).unwrap_or_else(|| "—".into());
        let count = |v: Option<usize>| v.map(|v| v.to_string()).unwrap_or_else(|| "—".into());
        format!(
            "Universe breadth · {} closes · >SMA50 {}%/{} · >SMA200 {}%/{} · 1d ↑{} ↓{} ={} /{} A/D {} · 5d ↑{} ↓{} ={} /{} A/D {}",
            self.n,
            num(self.pct_above_sma50),
            self.n_sma50,
            num(self.pct_above_sma200),
            self.n_sma200,
            count(self.adv_1d),
            count(self.dec_1d),
            count(self.unch_1d),
            self.n_ad_1d,
            num(self.ad_1d),
            count(self.adv_5d),
            count(self.dec_5d),
            count(self.unch_5d),
            self.n_ad_5d,
            num(self.ad_5d)
        )
    }
}

/// Breadth of histories already loaded for SCAN, including short histories and
/// names that will not match the selected setup.
pub fn history_breadth(histories: &[(String, Vec<tr_core::Bar>)]) -> Breadth {
    breadth(&histories.iter().map(|(_, bars)| bars.iter().map(|b| b.close).collect()).collect::<Vec<_>>())
}

/// Whats-News RSI lens: pandas adjust=False EWM, including missing-position weights.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RsiMetrics {
    pub rsi14: Option<f64>,
    pub rsi14_prev: Option<f64>,
    pub rsi_os: bool,
    pub rsi_ob: bool,
    pub rsi_rising_from_os: bool,
}
impl RsiMetrics {
    pub fn matches_rsi(&self) -> bool {
        self.rsi_os || self.rsi_ob || self.rsi_rising_from_os
    }
}
pub fn measure_rsi(close: &[f64]) -> RsiMetrics {
    if close.iter().filter(|v| !v.is_nan()).count() < 16 {
        return RsiMetrics::default();
    }
    let alpha = 1.0 / 14.0;
    let mut gain: Option<f64> = None;
    let mut loss: Option<f64> = None;
    let mut weight = 1.0;
    let mut count = 0;
    let mut values = Vec::new();
    let mut deltas = Vec::new();
    for pair in close.windows(2) {
        let delta = pair[1] - pair[0];
        // pandas EWM treats infinities as missing as well.
        let observed = delta.is_finite();
        if observed {
            deltas.push(delta);
            count += 1;
        }
        if let (Some(g), Some(l)) = (gain, loss) {
            weight *= 1.0 - alpha;
            if observed {
                let ng = delta.max(0.0);
                let nl = -delta.min(0.0);
                gain = Some(if g == ng { g } else { (weight * g + alpha * ng) / (weight + alpha) });
                loss = Some(if l == nl { l } else { (weight * l + alpha * nl) / (weight + alpha) });
                weight = 1.0;
            }
        } else if observed {
            gain = Some(delta.max(0.0));
            loss = Some(-delta.min(0.0));
        }
        if count >= 14 {
            if let (Some(g), Some(l)) = (gain, loss) {
                if l != 0.0 {
                    values.push(100.0 - 100.0 / (1.0 + g / l));
                }
            }
        }
    }
    let (cur, prev) = if let Some(last) = values.last() {
        (rounded(*last, 2), values.len().checked_sub(2).and_then(|i| rounded(values[i], 2)))
    } else {
        let limit = if deltas.len() < 14 {
            None
        } else if deltas.iter().all(|d| *d >= 0.0) {
            Some(100.0)
        } else if deltas.iter().all(|d| *d <= 0.0) {
            Some(0.0)
        } else {
            None
        };
        (limit, limit)
    };
    RsiMetrics {
        rsi14: cur,
        rsi14_prev: prev,
        rsi_os: cur.is_some_and(|v| v <= 30.0),
        rsi_ob: cur.is_some_and(|v| v >= 70.0),
        rsi_rising_from_os: cur.zip(prev).is_some_and(|(c, p)| p < 30.0 && c > p),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BreakoutMetrics {
    pub dist_52w_pct: Option<f64>,
    pub dist_nd_pct: Option<f64>,
    pub near_52w: bool,
    pub near_nd: bool,
    pub vol_ratio: Option<f64>,
    pub vol_surge: bool,
}
pub const NEAR_HIGH_PCT: f64 = -5.0;
pub const HIGH_ND: usize = 20;
pub const VOL_SURGE_X: f64 = 1.5;
impl BreakoutMetrics {
    pub fn matches_breakout(&self) -> bool { self.near_52w || self.near_nd || self.vol_surge }
}
pub fn measure_breakout(close: &[f64], high: &[f64], volume: &[f64]) -> BreakoutMetrics {
    let dist = |n: usize, minimum: usize| {
        let c = finite(*close.last()?)?;
        let window = &high[high.len().saturating_sub(n)..];
        if window.iter().filter(|v| !v.is_nan()).count() < minimum { return None; }
        let hi = window.iter().copied().filter(|v| !v.is_nan()).reduce(f64::max).and_then(finite)?;
        if hi <= 0.0 { return None; }
        rounded((c / hi - 1.0) * 100.0, 2)
    };
    let vol_ratio = (|| {
        if volume.len() < 3 { return None; }
        let today = finite(*volume.last()?)?;
        let prior: Vec<_> = volume[volume.len().saturating_sub(21)..volume.len()-1].iter().copied().filter(|v| !v.is_nan()).collect();
        let avg = finite(prior.iter().sum::<f64>() / prior.len() as f64)?;
        if avg <= 0.0 { return None; }
        rounded(today / avg, 2)
    })();
    let dist_52w_pct = dist(252, 60);
    let dist_nd_pct = dist(HIGH_ND, 10);
    BreakoutMetrics { dist_52w_pct, dist_nd_pct, vol_ratio,
        near_52w: dist_52w_pct.is_some_and(|v| v >= NEAR_HIGH_PCT),
        near_nd: dist_nd_pct.is_some_and(|v| v >= NEAR_HIGH_PCT),
        vol_surge: vol_ratio.is_some_and(|v| v >= VOL_SURGE_X) }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ScanBreadth {
    pub breadth: Breadth,
    pub symbols: usize,
    pub as_of: Option<chrono::NaiveDate>,
}
impl ScanBreadth {
    pub fn from_histories(histories: &[(String, Vec<tr_core::Bar>)]) -> Self {
        Self { breadth: history_breadth(histories), symbols: histories.len(),
            as_of: histories.iter().flat_map(|(_, bars)| bars.iter()).filter_map(|b| chrono::DateTime::from_timestamp(b.ts, 0)).max().map(|d| d.date_naive()) }
    }
}

/// Whats-News qulla lens; open falls back to close only when the column is absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct QullaMetrics {
    pub gap_pct: Option<f64>,
    pub is_ep: bool,
    pub qulla: bool,
}
pub const EP_GAP_PCT: f64 = 4.0;
pub fn measure_qulla(close: &[f64], open: Option<&[f64]>, breakout: &BreakoutMetrics) -> QullaMetrics {
    let prev = close.len().checked_sub(2).and_then(|i| finite(close[i]));
    let today_open = open.unwrap_or(close).last().copied().and_then(finite);
    let gap_pct = today_open.zip(prev).filter(|(o, p)| *o != 0.0 && *p != 0.0)
        .and_then(|(o, p)| rounded((o / p - 1.0) * 100.0, 2));
    let is_ep = gap_pct.is_some_and(|g| g >= EP_GAP_PCT) && breakout.vol_surge;
    QullaMetrics { gap_pct, is_ep, qulla: is_ep || breakout.vol_surge || breakout.near_nd }
}

/// O'Neil price/RS lens, matching Python's positional (not date-aligned) inputs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OneilMetrics {
    pub rs_spy_pct: Option<f64>,
    pub oneil_rs: bool,
}
pub const RS_BARS: usize = 63;
pub fn measure_oneil(close: &[f64], spy_close: Option<&[f64]>) -> OneilMetrics {
    let rs_spy_pct = (|| {
        let spy: Vec<_> = spy_close?.iter().copied().filter(|v| !v.is_nan()).collect();
        if close.len() <= RS_BARS || spy.len() <= RS_BARS
            || close.iter().filter(|v| !v.is_nan()).count() <= RS_BARS {
            return None;
        }
        // Symbol indexing retains NaN positions; SPY indexing follows dropna().
        let s0 = finite(close[close.len() - RS_BARS - 1])?;
        let s1 = finite(*close.last()?)?;
        let p0 = finite(spy[spy.len() - RS_BARS - 1])?;
        let p1 = finite(*spy.last()?)?;
        if s0 <= 0.0 || p0 <= 0.0 || s1 == 0.0 || p1 == 0.0 { return None; }
        rounded(((s1 / s0) / (p1 / p0) - 1.0) * 100.0, 2)
    })();
    OneilMetrics { rs_spy_pct, oneil_rs: rs_spy_pct.is_some_and(|v| v > 0.0) }
}

/// Honest VCP proxy using pandas-compatible range, Wilder ATR and swing contraction.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct VcpMetrics {
    pub range_ratio: Option<f64>,
    pub atr_declining: Option<bool>,
    pub swing_contract: Option<bool>,
    pub contracted: bool,
    pub vcp_proxy: bool,
}
pub const VCP_RANGE_RATIO: f64 = 0.55;
fn nan_extreme(values: &[f64], maximum: bool) -> Option<f64> {
    values.iter().copied().filter(|v| !v.is_nan())
        .reduce(if maximum { f64::max } else { f64::min })
}
pub fn range_ratio(high: &[f64], low: &[f64]) -> Option<f64> {
    if high.len() < 50 || low.len() < 50 { return None; }
    let range = |n| Some(finite(nan_extreme(&high[high.len()-n..], true)?)?
        - finite(nan_extreme(&low[low.len()-n..], false)?)?);
    let long = range(50)?;
    if long <= 0.0 { return None; }
    rounded(range(10)? / long, 3)
}
pub fn atr_declining(high: &[f64], low: &[f64], close: &[f64]) -> Option<bool> {
    if high.len().min(low.len()).min(close.len()) < 20 { return None; }
    let mut mean: Option<f64> = None;
    let mut weight = 1.0;
    let mut count = 0;
    let mut values = Vec::new();
    let alpha = 1.0 / 14.0;
    for i in 0..high.len().max(low.len()).max(close.len()) {
        let h = high.get(i).copied().unwrap_or(f64::NAN);
        let l = low.get(i).copied().unwrap_or(f64::NAN);
        let prev = i.checked_sub(1).and_then(|j| close.get(j)).copied().unwrap_or(f64::NAN);
        // DataFrame.max skips NaN, but retains infinity; EWM treats infinity as missing.
        let tr = nan_extreme(&[(h-l).abs(), (h-prev).abs(), (l-prev).abs()], true).and_then(finite);
        if tr.is_some() { count += 1; }
        if let Some(old) = mean {
            weight *= 1.0 - alpha;
            if let Some(x) = tr {
                mean = Some(if old == x { old } else { (weight * old + alpha * x) / (weight + alpha) });
                weight = 1.0;
            }
        } else if let Some(x) = tr { mean = Some(x); }
        // dropna retains carried EWM values at missing input positions.
        if count >= 14 { if let Some(x) = mean { values.push(x); } }
    }
    if values.len() < 6 { return None; }
    Some(finite(*values.last()?)? < finite(values[values.len()-6])?)
}
pub fn swing_ranges(high: &[f64], low: &[f64]) -> Vec<f64> {
    if high.len() < 7 || low.len() != high.len() { return Vec::new(); }
    let mut swings = Vec::new();
    for i in 2..high.len()-2 {
        if high[i].is_finite() && Some(high[i]) == nan_extreme(&high[i-2..=i+2], true) {
            swings.push(high[i]);
        } else if low[i].is_finite() && Some(low[i]) == nan_extreme(&low[i-2..=i+2], false) {
            swings.push(low[i]);
        }
    }
    swings.windows(2).map(|w| (w[1]-w[0]).abs()).filter(|v| v.is_finite() && *v > 0.0).collect()
}
pub fn swing_contract(high: &[f64], low: &[f64]) -> Option<bool> {
    let ranges = swing_ranges(high, low);
    let n = ranges.len();
    if n < 6 { return None; }
    let prior = ranges[n-6..n-3].iter().sum::<f64>() / 3.0;
    if prior <= 0.0 { return None; }
    Some(ranges[n-3..].iter().sum::<f64>() / 3.0 < prior)
}
pub fn measure_vcp(close: &[f64], high: &[f64], low: &[f64], breakout: &BreakoutMetrics) -> VcpMetrics {
    let range_ratio = range_ratio(high, low);
    let atr_declining = atr_declining(high, low, close);
    let swing_contract = swing_contract(high, low);
    let contracted = range_ratio.is_some_and(|r| r <= VCP_RANGE_RATIO)
        || atr_declining == Some(true) || swing_contract == Some(true);
    VcpMetrics { range_ratio, atr_declining, swing_contract, contracted,
        vcp_proxy: contracted && (breakout.near_52w || breakout.near_nd) }
}

/// Non-overlapping log-return block scaling from Whats-News fractal_scan.py.
pub const HORIZONS: [usize; 6] = [1, 2, 4, 8, 16, 32];
pub const WIN_65: usize = 65;
pub const WIN_130: usize = 130;
pub const FRAGILE_D: f64 = 1.40;
pub const FRAGILE_MOVE: f64 = 5.0;

fn valid_closes(close: &[f64]) -> bool {
    close.len() >= 2 && close.iter().all(|v| v.is_finite() && *v > 0.0)
}
#[allow(non_snake_case)]
pub fn hurst_H(close: &[f64]) -> Option<f64> {
    if !valid_closes(close) { return None; }
    let returns: Vec<_> = close.windows(2).map(|p| p[1].ln() - p[0].ln()).collect();
    let mut points = Vec::new();
    for k in HORIZONS {
        let blocks = returns.len() / k;
        if blocks < 2 { continue; }
        let mean = returns.chunks_exact(k).map(|b| b.iter().sum::<f64>().abs()).sum::<f64>() / blocks as f64;
        if mean > 0.0 && mean.is_finite() { points.push(((k as f64).ln(), mean.ln())); }
    }
    if points.len() < 3 { return None; }
    let x = points.iter().map(|p| p.0).sum::<f64>() / points.len() as f64;
    let y = points.iter().map(|p| p.1).sum::<f64>() / points.len() as f64;
    let variance = points.iter().map(|p| (p.0 - x).powi(2)).sum::<f64>();
    if variance <= 0.0 { return None; }
    finite(points.iter().map(|p| (p.0 - x) * (p.1 - y)).sum::<f64>() / variance)
}
#[allow(non_snake_case)]
pub fn hurst_D(close: &[f64]) -> Option<f64> { finite(2.0 - hurst_H(close)?) }
pub fn simple_move_pct(close: &[f64]) -> Option<f64> {
    if !valid_closes(close) { return None; }
    finite((close.last()? / close[0] - 1.0) * 100.0)
}
pub fn reading(d: Option<f64>, move_pct: Option<f64>) -> Option<&'static str> {
    let d = finite(d?)?;
    let movement = move_pct.and_then(finite).unwrap_or(0.0).abs();
    Some(if d <= FRAGILE_D && movement > FRAGILE_MOVE { "FRAGILE" }
        else if d <= 1.50 { "orderly" } else if d < 1.70 { "near-random" }
        else if d < 1.85 { "choppy" } else { "max chop" })
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FractalWindow {
    pub d: Option<f64>,
    pub h: Option<f64>,
    pub movement: Option<f64>,
    pub read: Option<String>,
}
pub fn window_pack(close: &[f64], n: usize) -> FractalWindow {
    if n < 2 || close.len() < n { return FractalWindow::default(); }
    let window = &close[close.len() - n..];
    if !valid_closes(window) { return FractalWindow::default(); }
    let h = hurst_H(window);
    let d = h.and_then(|h| finite(2.0 - h));
    let movement = simple_move_pct(window);
    FractalWindow { d: d.and_then(|v| rounded(v, 4)), h: h.and_then(|v| rounded(v, 4)),
        movement: movement.and_then(|v| rounded(v, 2)), read: reading(d, movement).map(str::to_owned) }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FragileMetrics {
    pub d_65d: Option<f64>,
    pub d_130d: Option<f64>,
    pub move_65d: Option<f64>,
    pub move_130d: Option<f64>,
    pub read_65d: Option<String>,
    pub read_130d: Option<String>,
    pub fragile: bool,
}
pub fn measure_fragile(close: &[f64]) -> FragileMetrics {
    let a = window_pack(close, WIN_65);
    let b = window_pack(close, WIN_130);
    FragileMetrics { fragile: a.read.as_deref() == Some("FRAGILE") || b.read.as_deref() == Some("FRAGILE"),
        d_65d: a.d, d_130d: b.d, move_65d: a.movement, move_130d: b.movement,
        read_65d: a.read, read_130d: b.read }
}
